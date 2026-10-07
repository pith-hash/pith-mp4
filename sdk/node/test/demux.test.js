// SPDX-License-Identifier: MIT
// Copyright (c) 2026 pith-hash
"use strict";

// Field-exact conformance: the committed reference vectors through koffi.
// Every vector in the repository-root reference.json is replayed against
// its committed fixture file and compared field-exact — the input's
// SHA-256/length, every demuxed fact (brands, timescales, per-sample
// records, stsd entries with avcC/esds hex) and the recorded refusal
// kinds, including the pinned truncated-prefix campaign. The same
// vectors the Rust gen-reference verify gate and the Python/Go SDKs
// check.

const test = require("node:test");
const assert = require("node:assert/strict");
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const {
  ERR_INVALID_MAGIC,
  ERR_UNSUPPORTED,
  FfiError,
  demuxCanonical,
  findCdylib,
  parseCanonical,
} = require("../index.js");

const REPO_ROOT = path.resolve(__dirname, "..", "..", "..");

const REFERENCE = JSON.parse(fs.readFileSync(path.join(REPO_ROOT, "reference.json"), "utf8"));
const VECTORS = REFERENCE.vectors;
const ERROR_VECTORS = REFERENCE.errors;

/** The committed fixture file a vector or error vector names. */
function fixtureBytes(name) {
  return fs.readFileSync(path.join(REPO_ROOT, "tests", "fixtures", `${name}.mp4`));
}

test("cdylib is discoverable", () => {
  assert.ok(fs.statSync(findCdylib()).isFile());
});

for (const vector of VECTORS) {
  test(`reference vector ${vector.name} is reproduced field-exact`, () => {
    const data = fixtureBytes(vector.name);

    // The fixture file is exactly the input the vector was computed
    // over: byte length and sha256 both pin it.
    assert.equal(data.length, vector.file_len, vector.name);
    assert.equal(
      crypto.createHash("sha256").update(data).digest("hex"),
      vector.file_sha256,
      vector.name,
    );

    const demuxed = parseCanonical(demuxCanonical(data));

    assert.equal(demuxed.majorBrand, vector.major_brand, vector.name);
    assert.deepEqual(demuxed.compatibleBrands, vector.compatible_brands, vector.name);
    assert.equal(demuxed.timescale, vector.timescale, vector.name);
    assert.equal(demuxed.duration, BigInt(vector.duration), vector.name);
    assert.equal(demuxed.tracks.length, vector.tracks.length, vector.name);

    demuxed.tracks.forEach((track, ti) => {
      const wantTrack = vector.tracks[ti];
      assert.equal(track.id, wantTrack.id, vector.name);
      assert.equal(track.timescale, wantTrack.timescale, vector.name);
      assert.equal(track.duration, BigInt(wantTrack.duration), vector.name);
      assert.equal(track.language, wantTrack.language, vector.name);
      assert.equal(track.handler, wantTrack.handler, vector.name);
      assert.equal(track.width, wantTrack.width, vector.name);
      assert.equal(track.height, wantTrack.height, vector.name);

      // Per-sample records, value-exact.
      assert.equal(track.samples.length, wantTrack.sample_count, vector.name);
      track.samples.forEach((sample, si) => {
        const want = wantTrack.samples[si];
        assert.equal(sample.offset, BigInt(want.offset), `${vector.name} sample ${si} offset`);
        assert.equal(sample.size, want.size, `${vector.name} sample ${si} size`);
        assert.equal(sample.decoding, BigInt(want.decoding), `${vector.name} sample ${si} dts`);
        assert.equal(
          sample.presentation,
          BigInt(want.presentation),
          `${vector.name} sample ${si} pts`,
        );
        assert.equal(sample.duration, want.duration, `${vector.name} sample ${si} duration`);
        assert.equal(sample.keyframe, want.keyframe, `${vector.name} sample ${si} sync`);
      });

      // stsd entries, including the avcC/esds payload hex. The JSON
      // records only the keys a kind carries (width/height for Visual,
      // channels/rate for Audio), so mirror that shape exactly.
      const entries = track.entries.map((entry) => {
        const shaped = {
          kind: entry.kind,
          coding: entry.coding,
          avcc_hex: entry.avccHex,
          esds_hex: entry.esdsHex,
        };
        if (entry.kind === "Visual") {
          shaped.width = entry.width;
          shaped.height = entry.height;
        }
        if (entry.kind === "Audio") {
          shaped.channels = entry.channels;
          shaped.rate = entry.rate;
        }
        return shaped;
      });
      assert.deepEqual(entries, wantTrack.stsd_entries, `${vector.name} stsd entries`);

      // The track-level avcC convenience copy.
      if ("avcc_hex" in wantTrack) {
        assert.equal(track.entries[0].avccHex, wantTrack.avcc_hex, vector.name);
      }
    });
  });
}

for (const vector of ERROR_VECTORS) {
  if ("error" in vector) {
    test(`error vector ${vector.name} is refused with the recorded kind`, () => {
      const data = fixtureBytes(vector.name);
      assert.equal(data.length, vector.file_len, vector.name);
      assert.throws(() => demuxCanonical(data), (err) => {
        assert.ok(err instanceof FfiError);
        assert.equal(err.status, -2, vector.name);
        const wantKind = vector.error === "Unsupported" ? ERR_UNSUPPORTED : ERR_INVALID_MAGIC;
        assert.equal(err.kind, wantKind, vector.name);
        return true;
      });
    });
  }
}

test("malformed input is refused, not crashing", () => {
  assert.throws(() => demuxCanonical(Buffer.from("not an mp4 file at all, really")), (err) => {
    assert.ok(err instanceof FfiError);
    assert.equal(err.status, -2);
    return true;
  });
});

test("empty input is refused", () => {
  assert.throws(() => demuxCanonical(Buffer.alloc(0)), FfiError);
});

test("truncated prefixes match the recorded campaign", () => {
  // Every prefix length the reference records for the minimal fixture
  // must refuse with the recorded kind — a status code, never a crash.
  const campaign = ERROR_VECTORS.find((v) => v.name === "truncated-prefixes-all");
  const data = fixtureBytes("minimal-4-samples");
  const kinds = { BadValue: 1, InvalidMagic: 2, TooLarge: 3, Truncated: 4, Unsupported: 5 };
  for (const run of campaign.runs) {
    assert.throws(() => demuxCanonical(data.subarray(0, run.prefix_len)), (err) => {
      assert.ok(err instanceof FfiError);
      assert.equal(err.status, -2, `prefix ${run.prefix_len}`);
      assert.equal(err.kind, kinds[run.error], `prefix ${run.prefix_len}`);
      return true;
    });
  }
});

test("fixture walk matches a rust-pinned digest", () => {
  // minimal-4-samples' record-walk digest, pinned in the Rust unit
  // tests; this test fails loudly even if the walk format drifted
  // between the crate and the SDK bindings.
  const demuxed = parseCanonical(demuxCanonical(fixtureBytes("minimal-4-samples")));
  assert.equal(
    crypto.createHash("sha256").update(demuxed.raw).digest("hex"),
    "be4b61fc669c451eb943875a47f2971daf92defbe20e58d9551ab29f76f89d24",
  );
  // Walk prologue: major brand isom, three compatible brands.
  assert.deepEqual([...demuxed.raw.subarray(0, 8)], [0x69, 0x73, 0x6f, 0x6d, 0, 0, 0, 3]);
});
