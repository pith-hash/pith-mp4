//! Regenerates `tests/reference.json`: hex-exact vectors pinning the
//! demuxed shape of deterministic hand-built ISO-BMFF fixtures.
//!
//! The fixture builders below mirror `tests/demux.rs` byte for byte; the
//! reference file records what `pith_mp4::demux` reports for each fixture
//! (track headers, per-sample records, `avcC` payload), the error variant
//! every malformed input produces, and an aggregate over a deterministic
//! SplitMix64 mutation campaign. `gen-reference` (no args) rewrites the
//! file; `gen-reference verify` regenerates in memory and compares
//! byte-for-byte against the committed copy — CI runs the verify mode.
//!
//! `gen-reference fixtures` writes the input files the vectors describe
//! to `tests/fixtures/<name>.mp4` (four success fixtures, three error
//! inputs), so the language SDKs can replay the vectors against real
//! bytes. The committed copies are self-verifying: every vector pins
//! its input's `file_sha256`, and a unit test below re-derives the
//! builder output and compares it byte-for-byte against what is
//! committed.

use std::process::ExitCode;

use pith_digest::{Error, SplitMix64, sha256};
use pith_mp4::{EntryKind, demux};

// ---------------------------------------------------------------- JSON value

/// Minimal JSON tree with insertion-ordered objects, so serialization is
/// byte-stable without pulling in a registry package.
#[derive(Clone, Debug)]
pub(crate) enum J {
    /// `null`.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A signed integer.
    Int(i64),
    /// A string (escaped on write).
    Str(String),
    /// An array.
    Arr(Vec<J>),
    /// An object; keys are written in insertion order.
    Obj(Vec<(String, J)>),
}

impl J {
    /// Object constructor helper.
    pub(crate) fn obj(entries: Vec<(&str, J)>) -> J {
        J::Obj(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    /// String constructor helper.
    pub(crate) fn s(v: impl Into<String>) -> J {
        J::Str(v.into())
    }

    /// Byte-string constructor helper: lower-case hex.
    pub(crate) fn hex(bytes: &[u8]) -> J {
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push_str(&format!("{b:02x}"));
        }
        J::Str(out)
    }

    /// Optional byte-string helper.
    pub(crate) fn opt_hex(bytes: Option<&[u8]>) -> J {
        match bytes {
            Some(b) => J::hex(b),
            None => J::Null,
        }
    }

    /// Serializes with two-space indentation and a trailing newline.
    pub(crate) fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    pub(crate) fn write(&self, out: &mut String, depth: usize) {
        let pad = "  ".repeat(depth);
        let pad_in = "  ".repeat(depth + 1);
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(n) => out.push_str(&n.to_string()),
            J::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => {
                            out.push_str(&format!("\\u{:04x}", c as u32));
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            J::Arr(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&pad_in);
                    item.write(out, depth + 1);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push(']');
            }
            J::Obj(entries) => {
                if entries.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push_str("{\n");
                for (i, (k, v)) in entries.iter().enumerate() {
                    out.push_str(&pad_in);
                    J::Str(k.clone()).write(out, depth + 1);
                    out.push_str(": ");
                    v.write(out, depth + 1);
                    if i + 1 < entries.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push('}');
            }
        }
    }
}

// ------------------------------------------------- fixture builders (mirror
// of tests/demux.rs — keep byte-identical)

/// `size:u32, four, payload`.
pub(crate) fn bx(four: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 8);
    v.extend_from_slice(&(payload.len() as u32 + 8).to_be_bytes());
    v.extend_from_slice(four);
    v.extend_from_slice(payload);
    v
}

/// `size:u32, four, version:u8, flags:u24, payload`.
pub(crate) fn full(four: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(payload.len() + 4);
    p.push(version);
    p.extend_from_slice(&flags.to_be_bytes()[1..]);
    p.extend_from_slice(payload);
    bx(four, &p)
}

pub(crate) fn u16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
pub(crate) fn u32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}
pub(crate) fn u64(v: u64) -> [u8; 8] {
    v.to_be_bytes()
}

/// The identity/unity matrix used by `mvhd`/`tkhd`.
const UNITY_MATRIX: [u8; 36] = [
    0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x01, 0x00, 0x00, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x00, 0x00, 0x00,
];

/// `ftyp` with major `isom`, minor 0, compat `isom,avc1,mp42`.
pub(crate) fn ftyp() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(b"isom");
    p.extend_from_slice(&u32(0));
    p.extend_from_slice(b"isom");
    p.extend_from_slice(b"avc1");
    p.extend_from_slice(b"mp42");
    bx(b"ftyp", &p)
}

/// `mvhd` v0: timescale 1000, duration 2000, unity matrix, rate/volume 1.0.
pub(crate) fn mvhd() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(0)); // creation
    p.extend_from_slice(&u32(0)); // modification
    p.extend_from_slice(&u32(1000)); // timescale
    p.extend_from_slice(&u32(2000)); // duration
    p.extend_from_slice(&u32(0x0001_0000)); // rate 1.0
    p.extend_from_slice(&u16(0x0100)); // volume 1.0
    p.extend_from_slice(&u16(0)); // reserved
    p.extend_from_slice(&u64(0)); // reserved
    p.extend_from_slice(&UNITY_MATRIX);
    p.extend_from_slice(&[0; 24]); // pre_defined
    p.extend_from_slice(&u32(2)); // next_track_id
    full(b"mvhd", 0, 0, &p)
}

/// `tkhd` v0 for track `id`, `w`×`h` display size, duration 2000.
pub(crate) fn tkhd(id: u32, w: u16, h: u16) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(0)); // creation
    p.extend_from_slice(&u32(0)); // modification
    p.extend_from_slice(&u32(id));
    p.extend_from_slice(&u32(0)); // reserved
    p.extend_from_slice(&u32(2000)); // duration
    p.extend_from_slice(&u64(0)); // reserved
    p.extend_from_slice(&u16(0)); // layer
    p.extend_from_slice(&u16(0)); // alternate group
    p.extend_from_slice(&u16(0)); // volume
    p.extend_from_slice(&u16(0)); // reserved
    p.extend_from_slice(&UNITY_MATRIX);
    p.extend_from_slice(&u32(u32::from(w) << 16)); // width 16.16
    p.extend_from_slice(&u32(u32::from(h) << 16));
    // flags: enabled | in_movie | in_preview
    full(b"tkhd", 0, 0x0000_0007, &p)
}

/// `mdhd` v0: timescale `ts`, duration `dur`, language `eng` (0x15C7).
pub(crate) fn mdhd(ts: u32, dur: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(0));
    p.extend_from_slice(&u32(0));
    p.extend_from_slice(&u32(ts));
    p.extend_from_slice(&u32(dur));
    // "eng" = 5<<10 | 14<<5 | 7 = 0x15C7; then pre_defined 0.
    p.extend_from_slice(&u16(0x15C7));
    p.extend_from_slice(&u16(0));
    full(b"mdhd", 0, 0, &p)
}

/// `hdlr` for the given handler type.
pub(crate) fn hdlr(kind: &[u8; 4]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(0)); // pre_defined
    p.extend_from_slice(kind);
    p.extend_from_slice(&[0; 12]); // reserved
    p.extend_from_slice(b"handler\0");
    full(b"hdlr", 0, 0, &p)
}

/// The `avcC` payload this suite uses everywhere: one fake SPS and one fake
/// PPS whose bytes are checked verbatim.
pub(crate) fn avcc_payload() -> Vec<u8> {
    let mut p = vec![
        1,    // configurationVersion
        66,   // AVCProfileIndication (baseline)
        0,    // profile_compatibility
        30,   // AVCLevelIndication
        0xFF, // lengthSizeMinusOne = 3 (4-byte NAL lengths)
        0xE1, // numOfSequenceParameterSets = 1
    ];
    p.extend_from_slice(&u16(4));
    p.extend_from_slice(&[0x67, 0x42, 0x00, 0x1E]); // SPS bytes
    p.push(1); // numOfPictureParameterSets
    p.extend_from_slice(&u16(3));
    p.extend_from_slice(&[0x68, 0xCE, 0x06]); // PPS bytes
    p
}

/// `stsd` with one `avc1` visual sample entry (`w`×`h`) carrying `avcC`.
pub(crate) fn stsd_avc1(w: u16, h: u16) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&[0; 6]); // reserved
    entry.extend_from_slice(&u16(1)); // data_reference_index
    entry.extend_from_slice(&u16(0)); // pre_defined
    entry.extend_from_slice(&u16(0)); // reserved
    entry.extend_from_slice(&[0; 12]); // pre_defined[3]
    entry.extend_from_slice(&u16(w));
    entry.extend_from_slice(&u16(h));
    entry.extend_from_slice(&u32(0x0048_0000)); // horizresolution 72dpi
    entry.extend_from_slice(&u32(0x0048_0000)); // vertresolution
    entry.extend_from_slice(&u32(0)); // reserved
    entry.extend_from_slice(&u16(1)); // frame_count
    let mut name = [0u8; 32];
    name[0] = 7;
    name[1..8].copy_from_slice(b"modhash");
    entry.extend_from_slice(&name);
    entry.extend_from_slice(&u16(0x0018)); // depth 24
    entry.extend_from_slice(&u16(0xFFFF)); // pre_defined -1
    entry.extend_from_slice(&bx(b"avcC", &avcc_payload()));
    let mut with_header = Vec::new();
    with_header.extend_from_slice(&u32(entry.len() as u32 + 8));
    with_header.extend_from_slice(b"avc1");
    with_header.extend_from_slice(&entry);
    let mut p = Vec::new();
    p.extend_from_slice(&u32(1)); // entry_count
    p.extend_from_slice(&with_header);
    full(b"stsd", 0, 0, &p)
}

pub(crate) fn stts(runs: &[(u32, u32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(runs.len() as u32));
    for &(count, delta) in runs {
        p.extend_from_slice(&u32(count));
        p.extend_from_slice(&u32(delta));
    }
    full(b"stts", 0, 0, &p)
}

/// `ctts` v1: one sample per run, `offsets[i]` is sample i's offset.
pub(crate) fn ctts_v1(offsets: &[i32]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(offsets.len() as u32));
    for &o in offsets {
        p.extend_from_slice(&u32(1));
        p.extend_from_slice(&u32(o as u32));
    }
    full(b"ctts", 1, 0, &p)
}

pub(crate) fn stsc(runs: &[(u32, u32, u32)]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(runs.len() as u32));
    for &(first, per, desc) in runs {
        p.extend_from_slice(&u32(first));
        p.extend_from_slice(&u32(per));
        p.extend_from_slice(&u32(desc));
    }
    full(b"stsc", 0, 0, &p)
}

pub(crate) fn stsz(sizes: &[u32]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(0)); // no uniform size
    p.extend_from_slice(&u32(sizes.len() as u32));
    for &s in sizes {
        p.extend_from_slice(&u32(s));
    }
    full(b"stsz", 0, 0, &p)
}

pub(crate) fn stss(indices: &[u32]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(indices.len() as u32));
    for &i in indices {
        p.extend_from_slice(&u32(i));
    }
    full(b"stss", 0, 0, &p)
}

pub(crate) fn stco(offsets: &[u64]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(offsets.len() as u32));
    for &o in offsets {
        p.extend_from_slice(&u32(o as u32));
    }
    full(b"stco", 0, 0, &p)
}

pub(crate) fn co64(offsets: &[u64]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(offsets.len() as u32));
    for &o in offsets {
        p.extend_from_slice(&u64(o));
    }
    full(b"co64", 0, 0, &p)
}

pub(crate) fn dinf() -> Vec<u8> {
    let url = full(b"url ", 0, 1, &[]); // self-contained
    let mut p = Vec::new();
    p.extend_from_slice(&u32(1));
    p.extend_from_slice(&url);
    let dref = full(b"dref", 0, 0, &p);
    bx(b"dinf", &dref)
}

pub(crate) fn vmhd() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u16(0)); // graphicsmode
    p.extend_from_slice(&[0; 6]); // opcolor
    full(b"vmhd", 0, 1, &p)
}

/// Composition offsets for the minimal file: sample 1 is shown one period
/// late, sample 2 one period early — the classic one-B-frame reorder.
pub(crate) fn ctts_offsets(n: u32) -> Vec<i32> {
    let mut v = vec![0i32; n as usize];
    if n >= 3 {
        v[1] = 45_000;
        v[2] = -45_000;
    }
    v
}

/// `stbl` for a track; `sync` is `None` to omit `stss` entirely.
pub(crate) fn stbl(
    offsets: &[u64],
    wide: bool,
    sizes: &[u32],
    runs: &[(u32, u32, u32)],
    sync: Option<&[u32]>,
) -> Vec<u8> {
    let n_samples = sizes.len() as u32;
    let mut v = Vec::new();
    v.extend_from_slice(&stsd_avc1(640, 360));
    v.extend_from_slice(&stts(&[(n_samples, 45_000)]));
    v.extend_from_slice(&stsc(runs));
    v.extend_from_slice(&stsz(sizes));
    v.extend_from_slice(&if wide { co64(offsets) } else { stco(offsets) });
    if let Some(list) = sync {
        v.extend_from_slice(&stss(list));
    }
    v.extend_from_slice(&ctts_v1(&ctts_offsets(n_samples)));
    bx(b"stbl", &v)
}

pub(crate) fn minf(
    offsets: &[u64],
    wide: bool,
    sizes: &[u32],
    runs: &[(u32, u32, u32)],
    sync: Option<&[u32]>,
) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&vmhd());
    v.extend_from_slice(&dinf());
    v.extend_from_slice(&stbl(offsets, wide, sizes, runs, sync));
    bx(b"minf", &v)
}

pub(crate) fn mdia(
    offsets: &[u64],
    wide: bool,
    sizes: &[u32],
    runs: &[(u32, u32, u32)],
    sync: Option<&[u32]>,
) -> Vec<u8> {
    let dur = 45_000 * sizes.len() as u32;
    let mut v = Vec::new();
    v.extend_from_slice(&mdhd(90_000, dur));
    v.extend_from_slice(&hdlr(b"vide"));
    v.extend_from_slice(&minf(offsets, wide, sizes, runs, sync));
    bx(b"mdia", &v)
}

pub(crate) fn trak(
    offsets: &[u64],
    wide: bool,
    sizes: &[u32],
    runs: &[(u32, u32, u32)],
    sync: Option<&[u32]>,
) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&tkhd(1, 640, 360));
    v.extend_from_slice(&mdia(offsets, wide, sizes, runs, sync));
    bx(b"trak", &v)
}

pub(crate) fn moov(
    offsets: &[u64],
    wide: bool,
    sizes: &[u32],
    runs: &[(u32, u32, u32)],
    sync: Option<&[u32]>,
) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&mvhd());
    v.extend_from_slice(&trak(offsets, wide, sizes, runs, sync));
    bx(b"moov", &v)
}

pub(crate) fn mdat(payload: &[u8]) -> Vec<u8> {
    bx(b"mdat", payload)
}

/// `mdat` with `size == 0`, running to end of file.
pub(crate) fn mdat_to_eof(payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&u32(0));
    v.extend_from_slice(b"mdat");
    v.extend_from_slice(payload);
    v
}

/// Assembles the fixture: `ftyp`, optional `free` largesize box, `moov`
/// (built twice — once to measure, once with real chunk offsets), then
/// `mdat`. `gap_bytes` are leading padding *inside* `mdat`'s payload so
/// chunk offsets are nontrivial, and `size0_mdat` switches `mdat` to the
/// to-end-of-file encoding. `sync = None` omits `stss`.
pub(crate) fn build_file(
    wide_offsets: bool,
    largesize_free_box: bool,
    size0_mdat: bool,
    gap_bytes: usize,
    sizes: &[u32],
    runs: &[(u32, u32, u32)],
    sync: Option<&[u32]>,
) -> Vec<u8> {
    let ftyp = ftyp();
    let free = if largesize_free_box {
        largesize_free()
    } else {
        Vec::new()
    };
    // moov's size does not depend on the offsets it contains (stco/co64
    // are fixed-width tables), so a probe with zeros measures where mdat
    // starts; chunk offsets point into mdat's payload after the gap.
    let probe = moov(
        &vec![0; n_chunks(sizes, runs)],
        wide_offsets,
        sizes,
        runs,
        sync,
    );
    let mdat_payload_start = ftyp.len() + free.len() + probe.len() + 8;
    let first_chunk = mdat_payload_start + gap_bytes;
    let offsets = chunk_offsets(first_chunk, sizes, runs);
    let moov = moov(&offsets, wide_offsets, sizes, runs, sync);
    let mut payload = vec![0xEE; gap_bytes];
    payload.extend_from_slice(&concat_samples(sizes));
    let mut file = ftyp;
    file.extend_from_slice(&free);
    file.extend_from_slice(&moov);
    file.extend_from_slice(&if size0_mdat {
        mdat_to_eof(&payload)
    } else {
        mdat(&payload)
    });
    file
}

/// A `free` box in the `size == 1` largesize encoding.
pub(crate) fn largesize_free() -> Vec<u8> {
    let payload = [0xAB; 10];
    let mut v = Vec::new();
    v.extend_from_slice(&u32(1)); // 64-bit size follows
    v.extend_from_slice(b"free");
    v.extend_from_slice(&u64(8 + 8 + payload.len() as u64));
    v.extend_from_slice(&payload);
    v
}

/// Concatenated sample payloads, in sample order: sample `i` is `size`
/// copies of byte `0x40 + i`.
pub(crate) fn concat_samples(sizes: &[u32]) -> Vec<u8> {
    let mut v = Vec::new();
    for (i, &s) in sizes.iter().enumerate() {
        v.extend(std::iter::repeat_n(0x40 + i as u8, s as usize));
    }
    v
}

/// Total chunk count implied by the stsc runs: how many chunks the table
/// needs to hold `sizes.len()` samples. Fixture callers shape runs so the
/// last run fills exactly the needed count.
pub(crate) fn n_chunks(sizes: &[u32], runs: &[(u32, u32, u32)]) -> usize {
    let mut remaining = sizes.len();
    let mut chunk = 0usize;
    for (i, &(first, per, _)) in runs.iter().enumerate() {
        let end = if i + 1 < runs.len() {
            runs[i + 1].0 as usize - 1
        } else {
            usize::MAX
        };
        let mut c = first as usize - 1;
        while c < end && remaining > 0 {
            remaining = remaining.saturating_sub(per as usize);
            c += 1;
            chunk = c;
            if remaining == 0 {
                return chunk;
            }
        }
    }
    chunk
}

/// Absolute file offsets of each chunk, laid out contiguously inside
/// `mdat` starting at `mdat_start` (the payload offset, after the header).
pub(crate) fn chunk_offsets(
    mdat_start: usize,
    sizes: &[u32],
    runs: &[(u32, u32, u32)],
) -> Vec<u64> {
    let n = n_chunks(sizes, runs);
    let mut offs = Vec::with_capacity(n);
    let mut at = mdat_start as u64;
    let mut next_sample = 0usize;
    for (i, &(first, per, _)) in runs.iter().enumerate() {
        let end = if i + 1 < runs.len() {
            runs[i + 1].0 as usize - 1
        } else {
            n
        };
        for c in (first as usize - 1)..end.min(n) {
            let _ = c;
            offs.push(at);
            let chunk_bytes: u64 = sizes
                [next_sample..(next_sample + per as usize).min(sizes.len())]
                .iter()
                .map(|&s| u64::from(s))
                .sum();
            at += chunk_bytes;
            next_sample += per as usize;
        }
    }
    while offs.len() < n {
        offs.push(at);
    }
    offs
}

/// `rng.below(n)` as a free helper: SplitMix64 carries `next_u64`/`fill_bytes`
/// only.
pub(crate) fn below(rng: &mut SplitMix64, n: usize) -> usize {
    if n == 0 {
        0
    } else {
        (rng.next_u64() % n as u64) as usize
    }
}

// ------------------------------------------------------------ vector bodies

/// Fourcc as a JSON string (ASCII four bytes).
pub(crate) fn four_json(four: &[u8; 4]) -> J {
    J::s(four.iter().map(|&b| b as char).collect::<String>())
}

/// Three-letter code (e.g. ISO-639-2 language) as a JSON string.
pub(crate) fn three_json(code: &[u8; 3]) -> J {
    J::s(code.iter().map(|&b| b as char).collect::<String>())
}

/// One demuxed file as a vector object.
pub(crate) fn demux_vector(name: &str, file: &[u8]) -> J {
    let mp4 = match demux(file) {
        Ok(m) => m,
        Err(e) => panic!("vector fixture `{name}` must demux, got {e:?}"),
    };
    let tracks = mp4
        .tracks
        .iter()
        .map(|t| {
            let samples: Vec<_> = t
                .samples()
                .collect::<Result<Vec<_>, _>>()
                .expect("vector fixture samples must expand");
            let entries = t.table.description.entries.iter().map(|e| match e {
                EntryKind::Visual {
                    coding,
                    width,
                    height,
                    avcc,
                    esds,
                } => J::obj(vec![
                    ("kind", J::s("Visual")),
                    ("coding", four_json(coding)),
                    ("width", J::Int(i64::from(*width))),
                    ("height", J::Int(i64::from(*height))),
                    ("avcc_hex", J::opt_hex(avcc.as_deref())),
                    ("esds_hex", J::opt_hex(esds.as_deref())),
                ]),
                EntryKind::Audio {
                    coding,
                    channels,
                    rate,
                    esds,
                } => J::obj(vec![
                    ("kind", J::s("Audio")),
                    ("coding", four_json(coding)),
                    ("channels", J::Int(i64::from(*channels))),
                    ("rate", J::Int(i64::from(*rate))),
                    ("esds_hex", J::opt_hex(esds.as_deref())),
                ]),
                EntryKind::Other { coding } => {
                    J::obj(vec![("kind", J::s("Other")), ("coding", four_json(coding))])
                }
            });
            J::obj(vec![
                ("id", J::Int(i64::from(t.id))),
                ("timescale", J::Int(i64::from(t.timescale))),
                ("duration", J::Int(t.duration as i64)),
                (
                    "language",
                    match t.language {
                        Some(l) => three_json(&l),
                        None => J::Null,
                    },
                ),
                ("handler", four_json(&t.handler)),
                ("width", J::Int(i64::from(t.width))),
                ("height", J::Int(i64::from(t.height))),
                ("sample_count", J::Int(t.table.len() as i64)),
                (
                    "samples",
                    J::Arr(
                        samples
                            .iter()
                            .map(|s| {
                                J::obj(vec![
                                    ("offset", J::Int(s.offset as i64)),
                                    ("size", J::Int(i64::from(s.size))),
                                    ("decoding", J::Int(s.decoding as i64)),
                                    ("presentation", J::Int(s.presentation)),
                                    ("duration", J::Int(i64::from(s.duration))),
                                    ("keyframe", J::Bool(s.keyframe)),
                                ])
                            })
                            .collect(),
                    ),
                ),
                ("stsd_entries", J::Arr(entries.collect())),
                ("avcc_hex", J::opt_hex(t.avcc())),
            ])
        })
        .collect();
    J::obj(vec![
        ("name", J::s(name)),
        (
            "file_sha256",
            J::hex(sha256(file).expect("sha256 cannot fail").as_bytes()),
        ),
        ("file_len", J::Int(file.len() as i64)),
        ("major_brand", four_json(&mp4.major_brand)),
        (
            "compatible_brands",
            J::Arr(mp4.compatible_brands.iter().map(four_json).collect()),
        ),
        ("timescale", J::Int(i64::from(mp4.timescale))),
        ("duration", J::Int(mp4.duration as i64)),
        ("tracks", J::Arr(tracks)),
    ])
}

/// Error-variant name for one `Error`, as recorded in the reference.
pub(crate) fn error_variant(e: &Error) -> &'static str {
    match e {
        Error::Truncated { .. } => "Truncated",
        Error::InvalidMagic { .. } => "InvalidMagic",
        Error::BadValue(_) => "BadValue",
        Error::Unsupported(_) => "Unsupported",
        Error::TooLarge { .. } => "TooLarge",
    }
}

/// A malformed input pinned only to its error variant.
pub(crate) fn error_vector(name: &str, file: &[u8]) -> J {
    let variant = match demux(file) {
        Ok(_) => panic!("error vector `{name}` must fail"),
        Err(e) => error_variant(&e),
    };
    J::obj(vec![
        ("name", J::s(name)),
        (
            "file_sha256",
            J::hex(sha256(file).expect("sha256 cannot fail").as_bytes()),
        ),
        ("file_len", J::Int(file.len() as i64)),
        ("error", J::s(variant)),
    ])
}

/// Every strict prefix of the minimal fixture must fail; the vector records
/// the variant each length produces.
pub(crate) fn truncated_prefixes_vector(minimal: &[u8]) -> J {
    let mut variants: Vec<J> = Vec::new();
    let mut counts = std::collections::BTreeMap::new();
    let mut ok_count = 0usize;
    for n in 0..minimal.len() {
        match demux(&minimal[..n]) {
            Ok(_) => ok_count += 1,
            Err(e) => {
                let v = error_variant(&e);
                *counts.entry(v).or_insert(0usize) += 1;
                if n == 0 || error_variant(&demux(&minimal[..n - 1]).unwrap_err()) != v {
                    // record only runs' starts to keep the file compact
                    variants.push(J::obj(vec![
                        ("prefix_len", J::Int(n as i64)),
                        ("error", J::s(v)),
                    ]));
                }
            }
        }
    }
    J::obj(vec![
        ("name", J::s("truncated-prefixes-all")),
        ("total_prefixes", J::Int(minimal.len() as i64)),
        ("ok_count", J::Int(ok_count as i64)),
        (
            "variant_counts",
            J::Obj(
                counts
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), J::Int(v as i64)))
                    .collect(),
            ),
        ),
        ("runs", J::Arr(variants)),
    ])
}

/// Numeric outcome code for one `Error`, folded into the campaign digest.
pub(crate) fn outcome_code(e: &Error) -> u8 {
    match e {
        Error::Truncated { .. } => 1,
        Error::InvalidMagic { .. } => 2,
        Error::BadValue(_) => 3,
        Error::Unsupported(_) => 4,
        Error::TooLarge { .. } => 5,
    }
}

/// Deterministic SplitMix64 mutation campaign over the minimal fixture,
/// mirroring `mutated_bytes_never_panic` in tests/demux.rs (same seed, same
/// iteration count, same per-iteration mutation arms). Records the outcome
/// mix plus an FNV-1a64 digest over the per-iteration outcome codes, so any
/// drift in parse behavior or error classification changes the reference.
pub(crate) fn fuzz_vector(minimal: &[u8]) -> J {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut rng = SplitMix64::new(0xdead_beef);
    let iterations = 20_000usize;
    let mut counts = std::collections::BTreeMap::new();
    let mut ok_count = 0usize;
    let mut digest = FNV_OFFSET;
    for _ in 0..iterations {
        let mut corrupt = minimal.to_vec();
        match rng.next_u64() % 4 {
            0 => {
                for _ in 0..1 + below(&mut rng, 8) {
                    let i = below(&mut rng, corrupt.len());
                    corrupt[i] ^= 1 << (rng.next_u64() % 8);
                }
            }
            1 => corrupt.truncate(below(&mut rng, corrupt.len() + 1)),
            2 => {
                if !corrupt.is_empty() {
                    let a = below(&mut rng, corrupt.len());
                    let b = below(&mut rng, corrupt.len());
                    let (lo, hi) = (a.min(b), a.max(b));
                    corrupt.drain(lo..hi.max(lo + 1).min(corrupt.len()));
                }
            }
            _ => {
                let at = below(&mut rng, corrupt.len() + 1);
                let v = rng.next_u64() as u8;
                for _ in 0..below(&mut rng, 16) {
                    corrupt.insert(at.min(corrupt.len()), v);
                }
            }
        }
        let (outcome, label) = match demux(&corrupt) {
            Ok(_) => {
                ok_count += 1;
                (0u8, "ok".to_string())
            }
            Err(e) => (outcome_code(&e), error_variant(&e).to_string()),
        };
        digest ^= u64::from(outcome);
        digest = digest.wrapping_mul(FNV_PRIME);
        *counts.entry(label).or_insert(0usize) += 1;
    }
    J::obj(vec![
        ("name", J::s("splitmix64-mutation-campaign")),
        ("seed_hex", J::s("0xdead_beef")),
        ("iterations", J::Int(iterations as i64)),
        ("ok_count", J::Int(ok_count as i64)),
        (
            "outcome_counts",
            J::Obj(
                counts
                    .into_iter()
                    .map(|(k, v)| (k, J::Int(v as i64)))
                    .collect(),
            ),
        ),
        ("outcome_digest", J::hex(&digest.to_be_bytes())),
    ])
}

/// The exact bytes of `tests/reference.json`.
pub(crate) fn reference_json() -> String {
    let minimal = build_file(
        false,
        false,
        false,
        16,
        &[3, 5, 3, 5],
        &[(1, 2, 1)],
        Some(&[1, 3]),
    );
    let root = J::obj(vec![
        ("generator", J::s("gen-reference")),
        ("format", J::Int(1)),
        (
            "vectors",
            J::Arr(vec![
                demux_vector("minimal-4-samples", &minimal),
                demux_vector(
                    "co64-variant",
                    &build_file(
                        true,
                        false,
                        false,
                        16,
                        &[3, 5, 3, 5],
                        &[(1, 2, 1)],
                        Some(&[1, 3]),
                    ),
                ),
                demux_vector(
                    "largesize-and-size0",
                    &build_file(
                        false,
                        true,
                        true,
                        8,
                        &[3, 5, 3, 5],
                        &[(1, 2, 1)],
                        Some(&[1, 3]),
                    ),
                ),
                demux_vector(
                    "stsc-multi-run",
                    &build_file(
                        false,
                        false,
                        false,
                        0,
                        &[4; 10],
                        &[(1, 3, 1), (3, 1, 1), (4, 2, 1)],
                        Some(&[1]),
                    ),
                ),
            ]),
        ),
        ("errors", {
            let ftyp_len = ftyp().len();
            let no_ftyp = minimal[ftyp_len..].to_vec();
            let mut weird = Vec::new();
            weird.extend_from_slice(b"xxxx");
            weird.extend_from_slice(&u32(0));
            weird.extend_from_slice(b"yyyy");
            let mut foreign = bx(b"ftyp", &weird);
            foreign.extend_from_slice(&minimal[ftyp_len..]);
            let mut fragmented = minimal.clone();
            fragmented.extend_from_slice(&bx(b"moof", &[]));
            J::Arr(vec![
                error_vector("no-ftyp", &no_ftyp),
                error_vector("foreign-brands-only", &foreign),
                error_vector("fragmented-moof", &fragmented),
                truncated_prefixes_vector(&minimal),
            ])
        }),
        ("fuzz", fuzz_vector(&minimal)),
    ]);
    root.to_json()
}

/// Reference file path: `<manifest>/reference.json` (repo root, the suite's
/// canonical location — matches pith-digest and pith-unicode).
pub(crate) fn reference_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("reference.json")
}

/// Compares freshly generated bytes against the committed file.
/// `Ok(())` when byte-identical; `Err(message)` describing the drift.
pub(crate) fn verify_against(committed: &str, generated: &str) -> Result<(), String> {
    if committed == generated {
        return Ok(());
    }
    let exp = committed.as_bytes();
    let got = generated.as_bytes();
    let at = exp
        .iter()
        .zip(got)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| exp.len().min(got.len()));
    Err(format!(
        "tests/reference.json is stale: expected {} bytes, generated {} bytes, first difference at byte {at}",
        exp.len(),
        got.len()
    ))
}

pub(crate) enum Mode {
    /// Rewrite `tests/reference.json` from the current vectors.
    Generate,
    /// Regenerate in memory and byte-compare against the committed file.
    Verify,
    /// Write the vector input files to `tests/fixtures/*.mp4`.
    Fixtures,
}

/// The directory the fixture files are written to, relative to the
/// crate root.
pub(crate) fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Every fixture file the reference vectors describe, as
/// `(file name, bytes)`, in a stable order: the four success fixtures
/// in vector order, then the three error inputs.
pub(crate) fn fixture_files() -> Vec<(&'static str, Vec<u8>)> {
    let minimal = build_file(
        false,
        false,
        false,
        16,
        &[3, 5, 3, 5],
        &[(1, 2, 1)],
        Some(&[1, 3]),
    );
    let ftyp_len = ftyp().len();
    let no_ftyp = minimal[ftyp_len..].to_vec();
    let mut weird = Vec::new();
    weird.extend_from_slice(b"xxxx");
    weird.extend_from_slice(&u32(0));
    weird.extend_from_slice(b"yyyy");
    let mut foreign = bx(b"ftyp", &weird);
    foreign.extend_from_slice(&minimal[ftyp_len..]);
    let mut fragmented = minimal.clone();
    fragmented.extend_from_slice(&bx(b"moof", &[]));
    vec![
        (
            "minimal-4-samples",
            build_file(
                false,
                false,
                false,
                16,
                &[3, 5, 3, 5],
                &[(1, 2, 1)],
                Some(&[1, 3]),
            ),
        ),
        (
            "co64-variant",
            build_file(
                true,
                false,
                false,
                16,
                &[3, 5, 3, 5],
                &[(1, 2, 1)],
                Some(&[1, 3]),
            ),
        ),
        (
            "largesize-and-size0",
            build_file(
                false,
                true,
                true,
                8,
                &[3, 5, 3, 5],
                &[(1, 2, 1)],
                Some(&[1, 3]),
            ),
        ),
        (
            "stsc-multi-run",
            build_file(
                false,
                false,
                false,
                0,
                &[4; 10],
                &[(1, 3, 1), (3, 1, 1), (4, 2, 1)],
                Some(&[1]),
            ),
        ),
        ("no-ftyp", no_ftyp),
        ("foreign-brands-only", foreign),
        ("fragmented-moof", fragmented),
    ]
}

/// Writes every fixture file into [`fixtures_dir`], creating the
/// directory as needed.
pub(crate) fn write_fixtures(dir: &std::path::Path) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create_dir_all {}: {e}", dir.display()))?;
    let files = fixture_files();
    for (name, bytes) in &files {
        let path = dir.join(format!("{name}.mp4"));
        std::fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(files.len())
}

pub(crate) fn run(path: &std::path::Path, mode: Mode) -> Result<(), String> {
    let generated = reference_json();
    match mode {
        Mode::Generate => {
            std::fs::write(path, &generated)
                .map_err(|e| format!("writing {}: {e}", path.display()))?;
            println!("wrote {} ({} bytes)", path.display(), generated.len());
            Ok(())
        }
        Mode::Verify => {
            let committed = std::fs::read_to_string(path)
                .map_err(|e| format!("reading {}: {e}", path.display()))?;
            verify_against(&committed, &generated)?;
            println!("reference.json is current");
            Ok(())
        }
        Mode::Fixtures => {
            let dir = fixtures_dir();
            let n = write_fixtures(&dir)?;
            println!("wrote {n} fixture files under {}", dir.display());
            Ok(())
        }
    }
}

/// CLI parsing: no argument generates, `verify` verifies, `fixtures`
/// writes the vector input files.
pub(crate) fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Mode, String> {
    match args.next().as_deref() {
        None => Ok(Mode::Generate),
        Some("verify") => Ok(Mode::Verify),
        Some("fixtures") => Ok(Mode::Fixtures),
        Some(other) => Err(format!(
            "usage: gen-reference [verify|fixtures] (got `{other}`)"
        )),
    }
}

pub(crate) fn main() -> ExitCode {
    let mode = match parse_args(std::env::args().skip(1)) {
        Ok(mode) => mode,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    match run(&reference_path(), mode) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pith_digest::SplitMix64;

    #[test]
    fn json_serializer_covers_every_arm() {
        // Str: every escape branch, including control characters.
        let s = J::obj(vec![("x", J::s("a\"b\\c\nd\re\tf\u{1}g"))]);
        assert_eq!(
            s.to_json(),
            "{\n  \"x\": \"a\\\"b\\\\c\\nd\\re\\tf\\u0001g\"\n}\n"
        );
        // Bool / Null / Int.
        assert_eq!(J::Bool(true).to_json(), "true\n");
        assert_eq!(J::Null.to_json(), "null\n");
        assert_eq!(J::Int(-7).to_json(), "-7\n");
        // Hex and opt-hex.
        assert_eq!(J::hex(&[0x0a, 0xff]).to_json(), "\"0aff\"\n");
        assert_eq!(J::opt_hex(None).to_json(), "null\n");
        assert_eq!(J::opt_hex(Some(&[0x01])).to_json(), "\"01\"\n");
        // Arr and Obj, empty and populated.
        assert_eq!(J::Arr(vec![]).to_json(), "[]\n");
        assert_eq!(J::Obj(vec![]).to_json(), "{}\n");
        let nested = J::obj(vec![("list", J::Arr(vec![J::Int(1), J::Int(2)]))]);
        assert!(nested.to_json().contains("\"list\": [\n    1,\n    2\n  ]"));
    }

    #[test]
    fn fourcc_and_threecc_and_below() {
        assert_eq!(four_json(b"avc1").to_json(), "\"avc1\"\n");
        assert_eq!(three_json(b"eng").to_json(), "\"eng\"\n");
        let mut rng = SplitMix64::new(9);
        assert_eq!(below(&mut rng, 0), 0);
        for _ in 0..32 {
            assert!(below(&mut rng, 5) < 5);
        }
    }

    #[test]
    fn box_helpers_render_known_bytes() {
        // `bx` sizes the payload; `full` adds version/flags; the scalar
        // encoders are big-endian.
        assert_eq!(&bx(b"free", &[])[..8], [0, 0, 0, 8, b'f', b'r', b'e', b'e']);
        assert_eq!(u16(1), [0, 1]);
        assert_eq!(u32(1), [0, 0, 0, 1]);
        assert_eq!(u64(1), [0, 0, 0, 0, 0, 0, 0, 1]);
        let f = full(b"stsd", 0, 0, &u32(0));
        assert_eq!(&f[4..8], b"stsd");
    }

    #[test]
    fn error_variant_and_outcome_code_map_every_variant() {
        let cases: [(Error, &str, u8); 5] = [
            (
                Error::Truncated {
                    what: "x",
                    needed: 1,
                    found: 0,
                },
                "Truncated",
                1,
            ),
            (Error::InvalidMagic { what: "x" }, "InvalidMagic", 2),
            (Error::BadValue("x"), "BadValue", 3),
            (Error::Unsupported("x"), "Unsupported", 4),
            (
                Error::TooLarge {
                    what: "x",
                    limit: 1,
                },
                "TooLarge",
                5,
            ),
        ];
        for (e, name, code) in cases {
            assert_eq!(error_variant(&e), name);
            assert_eq!(outcome_code(&e), code);
        }
    }

    #[test]
    fn error_vector_records_the_variant() {
        let v = error_vector("empty-input", &[]);
        let json = v.to_json();
        assert!(json.contains("\"error\": \"InvalidMagic\""), "{json}");
        let truncated = error_vector("short-input", b"no");
        assert!(
            truncated.to_json().contains("\"error\": \"Truncated\""),
            "{}",
            truncated.to_json()
        );
    }

    #[test]
    fn truncated_prefixes_vector_covers_every_length() {
        let files = fixture_files();
        let minimal = &files
            .iter()
            .find(|(n, _)| *n == "minimal-4-samples")
            .unwrap()
            .1;
        let v = truncated_prefixes_vector(minimal);
        let json = v.to_json();
        assert!(json.contains("\"total_prefixes\": 748"), "{json}");
    }

    #[test]
    fn fuzz_vector_is_deterministic() {
        let files = fixture_files();
        let minimal = &files
            .iter()
            .find(|(n, _)| *n == "minimal-4-samples")
            .unwrap()
            .1;
        let a = fuzz_vector(minimal);
        let b = fuzz_vector(minimal);
        assert_eq!(a.to_json(), b.to_json());
        assert!(a.to_json().contains("\"iterations\""));
    }

    #[test]
    fn fixture_files_are_seven_distinct_nonempty_inputs() {
        let files = fixture_files();
        assert_eq!(files.len(), 7);
        let mut names: Vec<_> = files.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let mut sorted = names.clone();
        sorted.dedup();
        assert_eq!(names, sorted);
        for (name, bytes) in &files {
            assert!(!bytes.is_empty(), "{name}");
            // The three error fixtures refuse; the four success fixtures
            // demux.
            if *name == "no-ftyp" || *name == "foreign-brands-only" || *name == "fragmented-moof" {
                assert!(demux(bytes).is_err(), "{name}");
            } else {
                assert!(demux(bytes).is_ok(), "{name}");
            }
        }
    }

    #[test]
    fn write_fixtures_roundtrips_through_a_tempdir() {
        let dir = std::env::temp_dir().join(format!(
            "pith-mp4-bin-fix-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(write_fixtures(&dir).unwrap(), 7);
        for (name, bytes) in fixture_files() {
            assert_eq!(
                std::fs::read(dir.join(format!("{name}.mp4"))).unwrap(),
                bytes,
                "{name}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_all_three_modes_roundtrip() {
        let path = std::env::temp_dir().join(format!(
            "pith-mp4-bin-run-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        run(&path, Mode::Generate).expect("generate");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), reference_json());
        run(&path, Mode::Verify).expect("verify own output");
        std::fs::write(&path, "stale").unwrap();
        assert!(run(&path, Mode::Verify).is_err());
        let dir = std::env::temp_dir();
        assert!(run(&dir, Mode::Generate).is_err());
        assert!(run(&dir, Mode::Verify).is_err());
        // Fixtures mode ignores the path and writes next to the crate.
        run(&path, Mode::Fixtures).expect("fixtures");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn parse_args_covers_every_mode() {
        assert!(matches!(
            parse_args(Vec::<String>::new().into_iter()),
            Ok(Mode::Generate)
        ));
        assert!(matches!(
            parse_args(["verify".to_string()].into_iter()),
            Ok(Mode::Verify)
        ));
        assert!(matches!(
            parse_args(["fixtures".to_string()].into_iter()),
            Ok(Mode::Fixtures)
        ));
        assert!(parse_args(["bogus".to_string()].into_iter()).is_err());
    }

    #[test]
    fn demux_vector_panics_on_non_file_input() {
        let result = std::panic::catch_unwind(|| demux_vector("not-an-mp4", &[1, 2, 3]));
        assert!(result.is_err());
    }

    #[test]
    fn vectors_serialize_audio_other_and_null_language() {
        // A minimal `mp4a` audio entry and a bare `Other` entry, driven
        // through `demux_vector` so the Audio/Other serialization arms
        // and the language fallback all execute.
        fn sample_entry(coding: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut entry = Vec::new();
            entry.extend_from_slice(&[0; 6]); // reserved
            entry.extend_from_slice(&u16(1)); // data_reference_index
            entry.extend_from_slice(&[0; 8]); // reserved[2]
            entry.extend_from_slice(body);
            let mut with_header = Vec::new();
            with_header.extend_from_slice(&u32(entry.len() as u32 + 8));
            with_header.extend_from_slice(coding);
            with_header.extend_from_slice(&entry);
            with_header
        }
        let audio = sample_entry(b"mp4a", &{
            let mut b = Vec::new();
            b.extend_from_slice(&u16(2)); // channels
            b.extend_from_slice(&u16(16)); // sample size
            b.extend_from_slice(&u16(0));
            b.extend_from_slice(&u16(0));
            b.extend_from_slice(&u32(44_100 << 16));
            b
        });
        let other = {
            let mut with_header = Vec::new();
            with_header.extend_from_slice(&u32(8));
            with_header.extend_from_slice(b"smpl");
            with_header
        };
        let mut stsd_body = u32(2).to_vec();
        stsd_body.extend_from_slice(&audio);
        stsd_body.extend_from_slice(&other);
        // The stco offset has to name the real mdat payload, so build
        // once to measure the fixed-width moov, then again with the
        // true offset (same byte width both times).
        let sizes = [3u32, 5, 3, 5];
        let payload = concat_samples(&sizes);
        let build = |mdat_payload_offset: u64| {
            let stbl_body = {
                let mut b = Vec::new();
                b.extend_from_slice(&full(b"stsd", 0, 0, &stsd_body));
                b.extend_from_slice(&stts(&[(4, 1)]));
                b.extend_from_slice(&stsc(&[(1, 4, 1)]));
                b.extend_from_slice(&stsz(&sizes));
                b.extend_from_slice(&stco(&[mdat_payload_offset]));
                bx(b"stbl", &b)
            };
            let mdia_body = {
                let mut b = Vec::new();
                b.extend_from_slice(&mdhd(90_000, 180_000));
                b.extend_from_slice(&hdlr(b"soun"));
                b.extend_from_slice(&bx(b"minf", &stbl_body));
                b
            };
            let mut f = ftyp();
            f.extend(bx(
                b"moov",
                &[
                    mvhd(),
                    bx(b"trak", &{
                        let mut b = Vec::new();
                        b.extend_from_slice(&tkhd(1, 0, 0));
                        b.extend_from_slice(&bx(b"mdia", &mdia_body));
                        b
                    }),
                ]
                .concat(),
            ));
            f.extend(mdat(&payload));
            f
        };
        // Rebuild using the measured layout: mdat payload starts right
        // after ftyp + moov + the 8-byte mdat header.
        let probe = build(0);
        let ftyp_len = ftyp().len();
        let moov_len = {
            // moov ends where mdat begins: file_len - payload - 8.
            probe.len() - payload.len() - 8 - ftyp_len
        };
        let file = build((ftyp_len + moov_len + 8) as u64);
        let json = demux_vector("mixed-entries", &file).to_json();
        assert!(json.contains("\"Audio\""), "{json}");
        assert!(json.contains("\"Other\""), "{json}");
        assert!(json.contains("\"channels\": 2"), "{json}");
        assert!(json.contains("\"rate\": 44100"), "{json}");
    }
}
