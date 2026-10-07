# SPDX-License-Identifier: MIT
# Copyright (c) 2026 pith-hash
"""pith-mp4 SDK: ISO-BMFF demuxing through ctypes.

Demuxes a native ISO-BMFF file (ftyp/moov/trak/stbl: sample tables,
`stts`/`stsc`/`stsz`/`stco`/`co64`/`stss`, `stsd` sample entries with
`avcC`/`esds`) through the Rust cdylib, handing back the canonical
record walk the ``reference.json`` vectors are checked against: the
demuxed shape serialized deterministically, field by field
(big-endian), exactly as the crate's ``ffi`` module documents it.

The cdylib is located through the suite's discovery chain:

1. ``PITH_CDYLIB`` — an explicit cdylib *file* path;
2. ``PITH_CDYLIB_DIR`` — a *directory* scanned for the cdylib names
   (the CD pipeline points this at ``target/release``);
3. the package directory itself (the built wheel ships the cdylib as
   package data);
4. ``<repo root>/target/release`` — the repository working-tree layout,
   so a source checkout runs against a local cargo build with no
   configuration.
"""

from __future__ import annotations

import ctypes
import os
from dataclasses import dataclass
from pathlib import Path

__all__ = [
    "Demuxed",
    "Track",
    "Sample",
    "Entry",
    "FfiError",
    "LibraryNotFoundError",
    "find_cdylib",
    "demux_canonical",
    "parse_canonical",
    "STATUS_OK",
    "STATUS_INVALID",
    "STATUS_REJECTED",
    "ERR_BAD_VALUE",
    "ERR_INVALID_MAGIC",
    "ERR_TOO_LARGE",
    "ERR_TRUNCATED",
    "ERR_UNSUPPORTED",
    "ENTRY_VISUAL",
    "ENTRY_AUDIO",
    "ENTRY_OTHER",
]

#: Status: success.
STATUS_OK = 0
#: Status: a caller argument is invalid (a null pointer).
STATUS_INVALID = -1
#: Status: the core demuxer refused the input (no ``ftyp``,
#: unrecognised brands, missing ``moov``, fragmented file, truncated).
STATUS_REJECTED = -2

#: Refusal kind ``"BadValue"`` (``Error::BadValue``).
ERR_BAD_VALUE = 1
#: Refusal kind ``"InvalidMagic"`` (``Error::InvalidMagic``).
ERR_INVALID_MAGIC = 2
#: Refusal kind ``"TooLarge"`` (``Error::TooLarge``).
ERR_TOO_LARGE = 3
#: Refusal kind ``"Truncated"`` (``Error::Truncated``).
ERR_TRUNCATED = 4
#: Refusal kind ``"Unsupported"`` (``Error::Unsupported``).
ERR_UNSUPPORTED = 5

#: ``stsd`` entry kind: a visual sample entry.
ENTRY_VISUAL = 0
#: ``stsd`` entry kind: an audio sample entry.
ENTRY_AUDIO = 1
#: ``stsd`` entry kind: neither visual nor audio.
ENTRY_OTHER = 2

#: Every cdylib file name cargo may drop into the build directory, per
#: platform (windows / linux / macOS).
CDYLIB_NAMES = ("pith_mp4.dll", "libpith_mp4.so", "libpith_mp4.dylib")

#: Refusal-kind names by code — the same stable names the ``errors``
#: of ``reference.json`` record as strings.
ERR_KIND_NAMES = {
    ERR_BAD_VALUE: "BadValue",
    ERR_INVALID_MAGIC: "InvalidMagic",
    ERR_TOO_LARGE: "TooLarge",
    ERR_TRUNCATED: "Truncated",
    ERR_UNSUPPORTED: "Unsupported",
}

#: ``stsd`` entry-kind names by code — the variant names reference.json
#: records under ``stsd_entries[].kind``.
ENTRY_KIND_NAMES = {ENTRY_VISUAL: "Visual", ENTRY_AUDIO: "Audio", ENTRY_OTHER: "Other"}


class LibraryNotFoundError(OSError):
    """No cdylib was found through the discovery chain."""


class FfiError(Exception):
    """A non-zero status code came back from the cdylib."""

    def __init__(self, op: str, status: int, kind: int = 0) -> None:
        detail = {
            STATUS_INVALID: "invalid argument",
            STATUS_REJECTED: "input rejected",
        }.get(status, "unknown failure")
        if kind:
            detail = f"{detail} ({ERR_KIND_NAMES.get(kind, f'kind {kind}')})"
        super().__init__(f"{op} failed: {detail} (status {status})")
        #: The raw status code the FFI returned.
        self.status = status
        #: The refusal kind code (0 unless the status is
        #: :data:`STATUS_REJECTED`; one of the ``ERR_*`` constants).
        self.kind = kind


def find_cdylib() -> Path:
    """Locates the cdylib through the suite's discovery chain."""
    explicit = os.environ.get("PITH_CDYLIB")
    if explicit:
        p = Path(explicit)
        if p.is_file():
            return p
    env_dir = os.environ.get("PITH_CDYLIB_DIR")
    candidates: list[Path] = []
    if env_dir:
        env_dir_path = Path(env_dir)
        candidates.append(env_dir_path)
        if not env_dir_path.is_absolute():
            # CD and local runs invoke tools from the repository root or
            # from sdk/<lang>; resolve the env value against both.
            candidates.append(Path.cwd() / env_dir_path)
            candidates.append(Path(__file__).resolve().parents[3] / env_dir_path)
    candidates.append(Path(__file__).resolve().parent)  # packaged wheel
    candidates.append(Path(__file__).resolve().parents[3] / "target" / "release")
    for directory in candidates:
        for name in CDYLIB_NAMES:
            p = directory / name
            if p.is_file():
                return p
    raise LibraryNotFoundError(
        "no pith-mp4 cdylib found (searched PITH_CDYLIB, PITH_CDYLIB_DIR, "
        "the package directory and <repo>/target/release); "
        "run `cargo build --release` first"
    )


_lib: ctypes.CDLL | None = None


def _load() -> ctypes.CDLL:
    global _lib
    if _lib is None:
        lib = ctypes.CDLL(str(find_cdylib()))
        lib.pith_mp4_demux.argtypes = [
            ctypes.c_void_p,  # data
            ctypes.c_size_t,  # len
            ctypes.POINTER(ctypes.c_void_p),  # out buffer
            ctypes.POINTER(ctypes.c_size_t),  # out length
            ctypes.POINTER(ctypes.c_int32),  # out refusal kind
        ]
        lib.pith_mp4_demux.restype = ctypes.c_int32
        lib.pith_mp4_free.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
        lib.pith_mp4_free.restype = None
        _lib = lib
    return _lib


def demux_canonical(data: bytes) -> bytes:
    """Demuxes a complete ISO-BMFF file into the canonical record walk
    the SDK vectors are checked against.

    Raises :class:`FfiError` with ``status == STATUS_REJECTED`` for any
    refused input; ``err.kind`` then carries one of the ``ERR_*`` kind
    codes the reference.json error vectors pin. The demuxer never
    panics through this boundary.
    """
    out = ctypes.c_void_p()
    out_len = ctypes.c_size_t()
    err = ctypes.c_int32()
    status = _load().pith_mp4_demux(
        data, len(data), ctypes.byref(out), ctypes.byref(out_len), ctypes.byref(err)
    )
    if status != STATUS_OK:
        raise FfiError("pith_mp4_demux", status, err.value)
    try:
        return ctypes.string_at(out, out_len.value)
    finally:
        _load().pith_mp4_free(out, out_len.value)


@dataclass(frozen=True)
class Sample:
    """One sample record of the canonical walk."""

    #: Absolute byte offset of the sample in the file.
    offset: int
    #: Sample size in bytes.
    size: int
    #: Decoding time in the track timescale.
    decoding: int
    #: Presentation time in the track timescale (may precede decoding
    #: with edit lists / B-frames).
    presentation: int
    #: Sample duration in the track timescale.
    duration: int
    #: Whether the sample is a sync (keyframe) sample.
    keyframe: bool


@dataclass(frozen=True)
class Entry:
    """One ``stsd`` sample entry of the canonical walk."""

    #: ``"Visual"``, ``"Audio"`` or ``"Other"``.
    kind: str
    #: The four-character coding (``avc1``, ``mp4a``, ...).
    coding: str
    #: Visual only: coded width.
    width: int | None
    #: Visual only: coded height.
    height: int | None
    #: Visual only: ``avcC`` payload, hex or ``None``.
    avcc_hex: str | None
    #: Audio only: channel count.
    channels: int | None
    #: Audio only: sample rate in Hz.
    rate: int | None
    #: ``esds`` payload, hex or ``None``.
    esds_hex: str | None


@dataclass(frozen=True)
class Track:
    """One demuxed track of the canonical walk."""

    #: Track id from ``tkhd``.
    id: int
    #: Media timescale from ``mdhd``.
    timescale: int
    #: Track duration in the media timescale.
    duration: int
    #: ISO-639-2/T language code, or ``None``.
    language: str | None
    #: Handler type (``vide``, ``soun``, ...).
    handler: str
    #: Visual track width from ``tkhd`` (0 for non-visual).
    width: int
    #: Visual track height from ``tkhd`` (0 for non-visual).
    height: int
    #: All sample records, in sample order.
    samples: list[Sample]
    #: All ``stsd`` sample entries.
    entries: list[Entry]


@dataclass(frozen=True)
class Demuxed:
    """A demuxed ISO-BMFF file, re-expressed from the canonical walk."""

    #: Major brand of the ``ftyp``.
    major_brand: str
    #: Compatible brands of the ``ftyp``.
    compatible_brands: list[str]
    #: Movie timescale from ``mvhd``.
    timescale: int
    #: Movie duration in the movie timescale.
    duration: int
    #: Demuxed tracks, in file order.
    tracks: list[Track]
    #: The canonical byte stream the walk is parsed from.
    raw: bytes


class _Reader:
    """Sequential big-endian reader over the canonical walk."""

    def __init__(self, raw: bytes) -> None:
        self.raw = raw
        self.pos = 0

    def take(self, n: int) -> bytes:
        if self.pos + n > len(self.raw):
            raise ValueError("canonical walk is truncated")
        chunk = self.raw[self.pos : self.pos + n]
        self.pos += n
        return chunk

    def u8(self) -> int:
        return self.take(1)[0]

    def u32(self) -> int:
        return int.from_bytes(self.take(4), "big")

    def u64(self) -> int:
        return int.from_bytes(self.take(8), "big")

    def i64(self) -> int:
        return int.from_bytes(self.take(8), "big", signed=True)

    def four(self) -> str:
        return self.take(4).decode("latin-1")


def _parse_entry(r: _Reader) -> Entry:
    kind_code = r.u8()
    coding = r.four()
    if kind_code == ENTRY_VISUAL:
        width = r.u32()
        height = r.u32()
        avcc = r.take(r.u32())
        esds = r.take(r.u32())
        return Entry(
            kind=ENTRY_KIND_NAMES[kind_code],
            coding=coding,
            width=width,
            height=height,
            avcc_hex=avcc.hex() if avcc else None,
            channels=None,
            rate=None,
            esds_hex=esds.hex() if esds else None,
        )
    if kind_code == ENTRY_AUDIO:
        channels = r.u32()
        rate = r.u32()
        esds = r.take(r.u32())
        return Entry(
            kind=ENTRY_KIND_NAMES[kind_code],
            coding=coding,
            width=None,
            height=None,
            avcc_hex=None,
            channels=channels,
            rate=rate,
            esds_hex=esds.hex() if esds else None,
        )
    return Entry(
        kind=ENTRY_KIND_NAMES[kind_code],
        coding=coding,
        width=None,
        height=None,
        avcc_hex=None,
        channels=None,
        rate=None,
        esds_hex=None,
    )


def _parse_track(r: _Reader) -> Track:
    track_id = r.u32()
    timescale = r.u32()
    duration = r.u64()
    language = None
    if r.u8() == 1:
        language = r.take(3).decode("latin-1")
    else:
        r.take(3)
    handler = r.four()
    width = r.u32()
    height = r.u32()
    samples = [
        Sample(
            offset=r.u64(),
            size=r.u32(),
            decoding=r.u64(),
            presentation=r.i64(),
            duration=r.u32(),
            keyframe=r.u8() == 1,
        )
        for _ in range(r.u32())
    ]
    entries = [_parse_entry(r) for _ in range(r.u32())]
    return Track(
        id=track_id,
        timescale=timescale,
        duration=duration,
        language=language,
        handler=handler,
        width=width,
        height=height,
        samples=samples,
        entries=entries,
    )


def parse_canonical(raw: bytes) -> Demuxed:
    """Re-expresses the canonical record walk as a :class:`Demuxed`."""
    r = _Reader(raw)
    major_brand = r.four()
    compatible_brands = [r.four() for _ in range(r.u32())]
    timescale = r.u32()
    duration = r.u64()
    tracks = [_parse_track(r) for _ in range(r.u32())]
    return Demuxed(
        major_brand=major_brand,
        compatible_brands=compatible_brands,
        timescale=timescale,
        duration=duration,
        tracks=tracks,
        raw=raw,
    )
