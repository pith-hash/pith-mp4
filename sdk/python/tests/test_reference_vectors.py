# SPDX-License-Identifier: MIT
# Copyright (c) 2026 pith-hash
"""Hex-exact conformance: the committed reference vectors through ctypes.

Every vector in the repository-root ``reference.json`` is replayed
against its committed fixture file and compared field-exact — the
input's SHA-256/length, every demuxed fact (brands, timescales,
per-sample records, ``stsd`` entries with ``avcC``/``esds`` hex) and
the recorded refusal kinds, including the pinned truncated-prefix
campaign. The same vectors the Rust ``gen-reference verify`` gate and
the Node/Go SDKs check.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import pytest

from pith_mp4 import (
    ERR_INVALID_MAGIC,
    ERR_UNSUPPORTED,
    FfiError,
    demux_canonical,
    find_cdylib,
    parse_canonical,
)

REPO_ROOT = Path(__file__).resolve().parents[3]
REFERENCE = json.loads((REPO_ROOT / "reference.json").read_text(encoding="utf-8"))
VECTORS = REFERENCE["vectors"]
ERROR_VECTORS = REFERENCE["errors"]


def fixture_bytes(name: str) -> bytes:
    """The committed fixture file a vector or error vector names."""
    return (REPO_ROOT / "tests" / "fixtures" / f"{name}.mp4").read_bytes()


def test_cdylib_is_discoverable() -> None:
    path = find_cdylib()
    assert path.is_file(), path


@pytest.mark.parametrize("vector", VECTORS, ids=lambda v: v["name"])
def test_reference_vector_is_reproduced_field_exact(vector: dict) -> None:
    data = fixture_bytes(vector["name"])

    # The fixture file is exactly the input the vector was computed
    # over: byte length and sha256 both pin it.
    assert len(data) == vector["file_len"], vector["name"]
    assert hashlib.sha256(data).hexdigest() == vector["file_sha256"], vector["name"]

    demuxed = parse_canonical(demux_canonical(data))

    assert demuxed.major_brand == vector["major_brand"], vector["name"]
    assert demuxed.compatible_brands == vector["compatible_brands"], vector["name"]
    assert demuxed.timescale == vector["timescale"], vector["name"]
    assert demuxed.duration == vector["duration"], vector["name"]
    assert len(demuxed.tracks) == len(vector["tracks"]), vector["name"]

    for track, want_track in zip(demuxed.tracks, vector["tracks"], strict=True):
        assert track.id == want_track["id"], vector["name"]
        assert track.timescale == want_track["timescale"], vector["name"]
        assert track.duration == want_track["duration"], vector["name"]
        assert track.language == want_track["language"], vector["name"]
        assert track.handler == want_track["handler"], vector["name"]
        assert track.width == want_track["width"], vector["name"]
        assert track.height == want_track["height"], vector["name"]

        # Per-sample records, value-exact.
        assert len(track.samples) == want_track["sample_count"], vector["name"]
        for sample, want in zip(track.samples, want_track["samples"], strict=True):
            assert (sample.offset, sample.size) == (want["offset"], want["size"]), vector["name"]
            assert sample.decoding == want["decoding"], vector["name"]
            assert sample.presentation == want["presentation"], vector["name"]
            assert sample.duration == want["duration"], vector["name"]
            assert sample.keyframe == want["keyframe"], vector["name"]

        # stsd entries, including the avcC/esds payload hex. The JSON
        # records only the keys a kind carries (width/height for
        # Visual, channels/rate for Audio), so mirror that shape.
        def shaped(entry):
            d = {
                "kind": entry.kind,
                "coding": entry.coding,
                "avcc_hex": entry.avcc_hex,
                "esds_hex": entry.esds_hex,
            }
            if entry.kind == "Visual":
                d["width"] = entry.width
                d["height"] = entry.height
            if entry.kind == "Audio":
                d["channels"] = entry.channels
                d["rate"] = entry.rate
            return d

        entries = [shaped(entry) for entry in track.entries]
        assert entries == want_track["stsd_entries"], vector["name"]

        # The track-level avcC convenience copy.
        if "avcc_hex" in want_track:
            assert track.entries[0].avcc_hex == want_track["avcc_hex"], vector["name"]


@pytest.mark.parametrize(
    "vector",
    [v for v in ERROR_VECTORS if "error" in v],
    ids=lambda v: v["name"],
)
def test_error_vector_is_refused_with_recorded_kind(vector: dict) -> None:
    data = fixture_bytes(vector["name"])
    assert hashlib.sha256(data).hexdigest() == vector["file_sha256"], vector["name"]
    assert len(data) == vector["file_len"], vector["name"]
    with pytest.raises(FfiError) as err:
        demux_canonical(data)
    assert err.value.status == -2, vector["name"]
    want_kind = ERR_UNSUPPORTED if vector["error"] == "Unsupported" else ERR_INVALID_MAGIC
    assert err.value.kind == want_kind, vector["name"]


def test_malformed_input_is_refused_not_crashing() -> None:
    with pytest.raises(FfiError) as err:
        demux_canonical(b"not an mp4 file at all, really")
    assert err.value.status == -2


def test_empty_input_is_refused() -> None:
    with pytest.raises(FfiError):
        demux_canonical(b"")


def test_truncated_prefixes_match_the_recorded_campaign() -> None:
    # Every prefix length the reference records for the minimal fixture
    # must refuse with the recorded kind — a status code, never a
    # crash.
    campaign = next(e for e in ERROR_VECTORS if e["name"] == "truncated-prefixes-all")
    data = fixture_bytes("minimal-4-samples")
    for run in campaign["runs"]:
        with pytest.raises(FfiError) as err:
            demux_canonical(data[: run["prefix_len"]])
        assert err.value.status == -2, run
        assert err.value.kind == {
            "BadValue": 1,
            "InvalidMagic": 2,
            "TooLarge": 3,
            "Truncated": 4,
            "Unsupported": 5,
        }[run["error"]], run


def test_fixture_walk_matches_a_rust_pinned_digest() -> None:
    # minimal-4-samples' record-walk digest, pinned in the Rust unit
    # tests; this test fails loudly even if the walk format drifted
    # between the crate and the SDK bindings.
    demuxed = parse_canonical(demux_canonical(fixture_bytes("minimal-4-samples")))
    assert hashlib.sha256(demuxed.raw).hexdigest() == (
        "be4b61fc669c451eb943875a47f2971daf92defbe20e58d9551ab29f76f89d24"
    )
    # Walk prologue: major brand isom, three compatible brands.
    assert demuxed.raw[:8] == b"isom\x00\x00\x00\x03"
