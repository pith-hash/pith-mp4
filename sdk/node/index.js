// SPDX-License-Identifier: MIT
// Copyright (c) 2026 pith-hash
"use strict";

/**
 * pith-mp4 SDK: ISO-BMFF demuxing through koffi.
 *
 * Demuxes a native ISO-BMFF file (ftyp/moov/trak/stbl: sample tables,
 * stts/stsc/stsz/stco/co64/stss and stsd sample entries with avcC and
 * esds) into the canonical record walk the `reference.json` vectors
 * are checked against: the demuxed shape serialized deterministically,
 * field by field (big-endian), exactly as the crate's `ffi` module
 * documents it.
 *
 * The cdylib is located through the suite's discovery chain:
 *
 *  1. `PITH_CDYLIB` — an explicit cdylib *file* path;
 *  2. `PITH_CDYLIB_DIR` — a *directory* scanned for the cdylib names
 *     (the CD pipeline points this at `target/release`);
 *  3. `prebuilds/` — the packaged npm layout the CD publish job
 *     assembles, flat and per `<os-arch>` (e.g. `linux-x64`);
 *  4. `<repo root>/target/release` — the repository working-tree
 *     layout, so a source checkout runs against a local cargo build
 *     with no configuration.
 *
 * The FFI surface is one demux operation plus one free:
 * `pith_mp4_demux` demuxes a whole ISO-BMFF file into the canonical
 * record walk, and `pith_mp4_free` releases the handed-out buffer.
 */

const koffi = require("koffi");
const fs = require("node:fs");
const path = require("node:path");

const STATUS_OK = 0;
const STATUS_INVALID = -1;
const STATUS_REJECTED = -2;

/** Refusal kind "BadValue" (Error::BadValue). */
const ERR_BAD_VALUE = 1;
/** Refusal kind "InvalidMagic" (Error::InvalidMagic). */
const ERR_INVALID_MAGIC = 2;
/** Refusal kind "TooLarge" (Error::TooLarge). */
const ERR_TOO_LARGE = 3;
/** Refusal kind "Truncated" (Error::Truncated). */
const ERR_TRUNCATED = 4;
/** Refusal kind "Unsupported" (Error::Unsupported). */
const ERR_UNSUPPORTED = 5;

/** stsd entry kinds, the codes the canonical walk carries. */
const ENTRY_VISUAL = 0;
const ENTRY_AUDIO = 1;
const ENTRY_OTHER = 2;

/** Refusal-kind names by code — the stable names reference.json
 * `errors` record as strings. */
const ERR_KIND_NAMES = Object.freeze({
  [ERR_BAD_VALUE]: "BadValue",
  [ERR_INVALID_MAGIC]: "InvalidMagic",
  [ERR_TOO_LARGE]: "TooLarge",
  [ERR_TRUNCATED]: "Truncated",
  [ERR_UNSUPPORTED]: "Unsupported",
});

/** stsd entry-kind names by code. */
const ENTRY_KIND_NAMES = Object.freeze({
  [ENTRY_VISUAL]: "Visual",
  [ENTRY_AUDIO]: "Audio",
  [ENTRY_OTHER]: "Other",
});

/** Every cdylib file name cargo may drop into the build directory, per platform. */
const CDYLIB_NAMES = ["pith_mp4.dll", "libpith_mp4.so", "libpith_mp4.dylib"];

const PKG_ROOT = path.join(__dirname);
const REPO_ROOT = path.resolve(__dirname, "..", "..");

/** FfiError: a non-zero status code came back from the cdylib. */
class FfiError extends Error {
  /**
   * @param {string} op the FFI operation name
   * @param {number} status the raw status code
   * @param {number} kind the refusal kind code (0 unless rejected)
   */
  constructor(op, status, kind = 0) {
    let kinded = { [STATUS_INVALID]: "invalid argument", [STATUS_REJECTED]: "input rejected" }[status] ?? "unknown failure";
    if (kind) kinded += ` (${ERR_KIND_NAMES[kind] ?? `kind ${kind}`})`;
    super(`${op} failed: ${kinded} (status ${status})`);
    this.name = "FfiError";
    /** The raw status code the FFI returned. */
    this.status = status;
    /** The refusal kind code (0 unless status is STATUS_REJECTED). */
    this.kind = kind;
  }
}

/**
 * Locates the cdylib through the suite's discovery chain.
 * @returns {string} an absolute path to the cdylib file
 * @throws {Error} when nothing is found
 */
function findCdylib() {
  const explicit = process.env.PITH_CDYLIB;
  if (explicit && fs.statSync(explicit, { throwIfNoEntry: false })?.isFile()) {
    return path.resolve(explicit);
  }
  /** @type {string[]} */
  const dirs = [];
  const envDir = process.env.PITH_CDYLIB_DIR;
  if (envDir) {
    dirs.push(envDir);
    if (!path.isAbsolute(envDir)) {
      dirs.push(path.join(REPO_ROOT, envDir));
    }
  }
  const osArch = `${process.platform}-${process.arch}`;
  dirs.push(path.join(PKG_ROOT, "prebuilds", osArch));
  dirs.push(path.join(PKG_ROOT, "prebuilds"));
  dirs.push(path.join(REPO_ROOT, "target", "release"));
  for (const dir of dirs) {
    for (const name of CDYLIB_NAMES) {
      const p = path.join(dir, name);
      if (fs.statSync(p, { throwIfNoEntry: false })?.isFile()) return p;
    }
  }
  throw new Error(
    "no pith-mp4 cdylib found (searched PITH_CDYLIB, PITH_CDYLIB_DIR, prebuilds/ and <repo>/target/release); " +
      "run `cargo build --release` first",
  );
}

let cached = undefined;

/**
 * Loads the cdylib and binds the exported symbols (lazily, once).
 * @returns {{demux: Function, free: Function}}
 */
function loadLibrary() {
  if (cached) return cached;
  const lib = koffi.load(findCdylib());
  const demux = lib.func("pith_mp4_demux", "int32_t", [
    "const uint8_t *",
    "size_t",
    koffi.out(koffi.pointer("void *")),
    koffi.out(koffi.pointer("size_t")),
    koffi.out(koffi.pointer("int32_t")),
  ]);
  const free = lib.func("void pith_mp4_free(void *ptr, size_t len)");
  cached = { demux, free };
  return cached;
}

/**
 * Demuxes a complete ISO-BMFF file into the canonical record walk the
 * SDK vectors are checked against. The handed-out cdylib buffer is
 * copied into a JS Buffer and released before returning.
 *
 * @param {Buffer} data the complete file bytes
 * @returns {Buffer} the canonical record walk
 * @throws {FfiError} with `status === -2` for any refused input
 */
function demuxCanonical(data) {
  if (!Buffer.isBuffer(data)) {
    throw new TypeError("data must be a Buffer");
  }
  const { demux, free } = loadLibrary();
  const out = [null];
  const outLen = [0];
  const err = [0];
  const status = demux(data, data.length, out, outLen, err);
  if (status !== STATUS_OK) {
    throw new FfiError("pith_mp4_demux", status, err[0]);
  }
  try {
    // koffi.decode hands back a Uint8Array view over the external
    // buffer; copy it into a Buffer before the cdylib buffer is freed.
    return Buffer.from(koffi.decode(out[0], "uint8_t", Number(outLen[0])));
  } finally {
    free(out[0], Number(outLen[0]));
  }
}

/** Sequential big-endian reader over the canonical walk. */
class Reader {
  /**
   * @param {Buffer} raw the canonical walk
   */
  constructor(raw) {
    this.raw = raw;
    this.pos = 0;
  }
  /**
   * @param {number} n bytes to consume
   * @returns {Buffer}
   */
  take(n) {
    if (this.pos + n > this.raw.length) {
      throw new Error("canonical walk is truncated");
    }
    const chunk = this.raw.subarray(this.pos, this.pos + n);
    this.pos += n;
    return chunk;
  }
  /** @returns {number} */
  u8() {
    return this.take(1)[0];
  }
  /** @returns {number} */
  u32() {
    return this.take(4).readUInt32BE(0);
  }
  /** @returns {bigint} */
  u64() {
    return this.take(8).readBigUInt64BE(0);
  }
  /** @returns {bigint} signed 64-bit (presentation times). */
  i64() {
    return this.take(8).readBigInt64BE(0);
  }
  /** @returns {string} a four-character code. */
  four() {
    return this.take(4).toString("latin1");
  }
}

/**
 * Parses one stsd entry of the walk.
 * @param {Reader} r
 * @returns {{kind: string, coding: string, width: number|null, height: number|null,
 *   avccHex: string|null, channels: number|null, rate: number|null, esdsHex: string|null}}
 */
function parseEntry(r) {
  const kindCode = r.u8();
  const coding = r.four();
  if (kindCode === ENTRY_VISUAL) {
    const width = r.u32();
    const height = r.u32();
    const avcc = r.take(Number(r.u32()));
    const esds = r.take(Number(r.u32()));
    return {
      kind: ENTRY_KIND_NAMES[kindCode],
      coding,
      width,
      height,
      avccHex: avcc.length ? avcc.toString("hex") : null,
      channels: null,
      rate: null,
      esdsHex: esds.length ? esds.toString("hex") : null,
    };
  }
  if (kindCode === ENTRY_AUDIO) {
    const channels = r.u32();
    const rate = r.u32();
    const esds = r.take(Number(r.u32()));
    return {
      kind: ENTRY_KIND_NAMES[kindCode],
      coding,
      width: null,
      height: null,
      avccHex: null,
      channels,
      rate,
      esdsHex: esds.length ? esds.toString("hex") : null,
    };
  }
  return {
    kind: ENTRY_KIND_NAMES[kindCode],
    coding,
    width: null,
    height: null,
    avccHex: null,
    channels: null,
    rate: null,
    esdsHex: null,
  };
}

/**
 * Parses one track of the walk.
 * @param {Reader} r
 * @returns {{id: number, timescale: number, duration: bigint, language: string|null,
 *   handler: string, width: number, height: number, samples: object[], entries: object[]}}
 */
function parseTrack(r) {
  const id = r.u32();
  const timescale = r.u32();
  const duration = r.u64();
  let language = null;
  if (r.u8() === 1) {
    language = r.take(3).toString("latin1");
  } else {
    r.take(3);
  }
  const handler = r.four();
  const width = r.u32();
  const height = r.u32();
  const sampleCount = Number(r.u32());
  const samples = [];
  for (let i = 0; i < sampleCount; i++) {
    samples.push({
      offset: r.u64(),
      size: r.u32(),
      decoding: r.u64(),
      presentation: r.i64(),
      duration: r.u32(),
      keyframe: r.u8() === 1,
    });
  }
  const entryCount = Number(r.u32());
  const entries = [];
  for (let i = 0; i < entryCount; i++) {
    entries.push(parseEntry(r));
  }
  return { id, timescale, duration, language, handler, width, height, samples, entries };
}

/**
 * Re-expresses the canonical record walk as a plain object.
 *
 * @param {Buffer} raw the canonical walk
 * @returns {{majorBrand: string, compatibleBrands: string[], timescale: number,
 *   duration: bigint, tracks: object[], raw: Buffer}}
 */
function parseCanonical(raw) {
  if (!Buffer.isBuffer(raw)) {
    throw new TypeError("raw must be a Buffer");
  }
  const r = new Reader(raw);
  const majorBrand = r.four();
  const brandCount = Number(r.u32());
  const compatibleBrands = [];
  for (let i = 0; i < brandCount; i++) {
    compatibleBrands.push(r.four());
  }
  const timescale = r.u32();
  const duration = r.u64();
  const trackCount = Number(r.u32());
  const tracks = [];
  for (let i = 0; i < trackCount; i++) {
    tracks.push(parseTrack(r));
  }
  return { majorBrand, compatibleBrands, timescale, duration, tracks, raw };
}

module.exports = {
  STATUS_OK,
  STATUS_INVALID,
  STATUS_REJECTED,
  ERR_BAD_VALUE,
  ERR_INVALID_MAGIC,
  ERR_TOO_LARGE,
  ERR_TRUNCATED,
  ERR_UNSUPPORTED,
  ENTRY_VISUAL,
  ENTRY_AUDIO,
  ENTRY_OTHER,
  ERR_KIND_NAMES,
  ENTRY_KIND_NAMES,
  CDYLIB_NAMES,
  FfiError,
  findCdylib,
  demuxCanonical,
  parseCanonical,
};
