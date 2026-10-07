//! Tests for the `gen-reference` tool and edge-path coverage for the
//! demuxer: the tool's serializer/verify/CLI surface is exercised through a
//! module include of `tools/gen-reference/main.rs`, and the demuxer's
//! error/variant paths are driven with hand-built boxes that reuse the
//! tool's fixture builders.

#[allow(dead_code)]
#[path = "../tools/gen-reference/main.rs"]
mod tool;

use std::process::Command;

use tool::{
    J, Mode, avcc_payload, below, build_file, bx, chunk_offsets, concat_samples, demux_vector,
    dinf, error_variant, four_json, ftyp, full, hdlr, mdat, mdhd, minf, moov, mvhd, outcome_code,
    parse_args, reference_json, run, stco, stsc, stsd_avc1, stss, stsz, stts, three_json, tkhd,
    u16, u32, u64, verify_against, vmhd,
};

// ------------------------------------------------------------ tool surface

#[test]
fn serializer_is_stable_and_escapes() {
    let v = J::obj(vec![
        ("b", J::Int(-3)),
        (
            "a",
            J::Arr(vec![J::Bool(true), J::Null, J::s("x\"y\n\t\r\u{7}\\")]),
        ),
        ("empty", J::Obj(vec![])),
    ]);
    assert_eq!(
        v.to_json(),
        "{\n  \"b\": -3,\n  \"a\": [\n    true,\n    null,\n    \"x\\\"y\\n\\t\\r\\u0007\\\\\"\n  \
         ],\n  \"empty\": {}\n}\n"
    );
}

#[test]
fn fourcc_and_threecc_helpers_render_ascii() {
    assert_eq!(four_json(b"avc1").to_json(), "\"avc1\"\n");
    assert_eq!(three_json(b"eng").to_json(), "\"eng\"\n");
}

#[test]
fn below_handles_empty_range() {
    let mut rng = pith_digest::SplitMix64::new(7);
    assert_eq!(below(&mut rng, 0), 0);
    assert!(below(&mut rng, 3) < 3);
}

#[test]
fn reference_generation_is_deterministic() {
    let a = reference_json();
    let b = reference_json();
    assert_eq!(a, b);
    assert!(a.contains("\"minimal-4-samples\""));
    assert!(a.contains("\"splitmix64-mutation-campaign\""));
    assert!(a.ends_with('\n'));
}

#[test]
fn fixtures_are_byte_stable() {
    let a = build_file(
        false,
        false,
        false,
        16,
        &[3, 5, 3, 5],
        &[(1, 2, 1)],
        Some(&[1, 3]),
    );
    let b = build_file(
        false,
        false,
        false,
        16,
        &[3, 5, 3, 5],
        &[(1, 2, 1)],
        Some(&[1, 3]),
    );
    assert_eq!(a, b);
}

#[test]
fn verify_accepts_current_and_rejects_drift() {
    let generated = reference_json();
    assert!(verify_against(&generated, &generated).is_ok());
    let drifted = generated.replace("\"format\": 1", "\"format\": 2");
    assert!(verify_against(&drifted, &generated).is_err());
    assert!(verify_against(&generated, &generated[..generated.len() - 1]).is_err());
}

#[test]
fn error_variant_and_outcome_code_cover_every_variant() {
    let cases: [(pith_digest::Error, &str, u8); 5] = [
        (
            pith_digest::Error::Truncated {
                what: "x",
                needed: 1,
                found: 0,
            },
            "Truncated",
            1,
        ),
        (
            pith_digest::Error::InvalidMagic { what: "x" },
            "InvalidMagic",
            2,
        ),
        (pith_digest::Error::BadValue("x"), "BadValue", 3),
        (pith_digest::Error::Unsupported("x"), "Unsupported", 4),
        (
            pith_digest::Error::TooLarge {
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
fn parse_args_modes() {
    let none = Vec::<String>::new().into_iter();
    assert!(matches!(parse_args(none), Ok(Mode::Generate)));
    let verify = ["verify".to_string()].into_iter();
    assert!(matches!(parse_args(verify), Ok(Mode::Verify)));
    let junk = ["bogus".to_string()].into_iter();
    assert!(parse_args(junk).is_err());
}

#[test]
fn run_generate_then_verify_roundtrip() {
    let path = std::env::temp_dir().join(format!(
        "pith-mp4-ref-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    run(&path, Mode::Generate).expect("generate must write");
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written, reference_json());
    run(&path, Mode::Verify).expect("verify must accept its own output");
    // Drift against the file must fail verify...
    std::fs::write(&path, "stale").unwrap();
    assert!(run(&path, Mode::Verify).is_err());
    // ...and unwritable targets must fail both modes.
    let dir = std::env::temp_dir();
    assert!(run(&dir, Mode::Generate).is_err());
    assert!(run(&dir, Mode::Verify).is_err());
    std::fs::remove_file(&path).ok();
}

#[test]
#[should_panic(expected = "must demux")]
fn demux_vector_panics_on_non_file() {
    demux_vector("not-an-mp4", &[1, 2, 3]);
}

#[test]
fn committed_reference_verifies_end_to_end() {
    // Runs the real committed binary exactly the way CI does.
    let status = Command::new(env!("CARGO_BIN_EXE_gen-reference"))
        .arg("verify")
        .status()
        .expect("binary must run");
    assert!(status.success());
    let bad = Command::new(env!("CARGO_BIN_EXE_gen-reference"))
        .arg("nonsense")
        .status()
        .expect("binary must run");
    assert!(!bad.success());
}

// ------------------------------------------------- demuxer edge-path coverage
//
// The builders below compose the tool's fixture primitives into malformed or
// variant files the main suite does not cover.

/// Composes a one-track file from custom `moov` children.
fn file_from_moov(moov_children: &[Vec<u8>]) -> Vec<u8> {
    let mut inner = Vec::new();
    for child in moov_children {
        inner.extend(child);
    }
    let mut file = ftyp();
    file.extend(bx(b"moov", &inner));
    file
}

/// Standard video track with a caller-supplied `stbl` body.
fn video_trak(stbl_body: &[u8]) -> Vec<u8> {
    let mut mdia_body = Vec::new();
    mdia_body.extend(mdhd(90_000, 180_000));
    mdia_body.extend(hdlr(b"vide"));
    let mut minf_body = Vec::new();
    minf_body.extend(vmhd());
    minf_body.extend(dinf());
    minf_body.extend(bx(b"stbl", stbl_body));
    mdia_body.extend(bx(b"minf", &minf_body));
    let mut trak_body = Vec::new();
    trak_body.extend(tkhd(1, 640, 360));
    trak_body.extend(bx(b"mdia", &mdia_body));
    bx(b"trak", &trak_body)
}

/// `stsd` with caller-chosen sample entries.
fn stsd_with(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(entries.len() as u32));
    for entry in entries {
        p.extend(entry);
    }
    full(b"stsd", 0, 0, &p)
}

/// Visual sample entry (`coding`) of `w`×`h` with extra child boxes.
fn visual_entry(coding: &[u8; 4], w: u16, h: u16, children: &[Vec<u8>]) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&[0; 6]); // reserved
    entry.extend_from_slice(&u16(1)); // data_reference_index
    entry.extend_from_slice(&u16(0)); // pre_defined
    entry.extend_from_slice(&u16(0)); // reserved
    entry.extend_from_slice(&[0; 12]); // pre_defined[3]
    entry.extend_from_slice(&u16(w));
    entry.extend_from_slice(&u16(h));
    entry.extend_from_slice(&u32(0x0048_0000));
    entry.extend_from_slice(&u32(0x0048_0000));
    entry.extend_from_slice(&u32(0));
    entry.extend_from_slice(&u16(1)); // frame_count
    entry.extend_from_slice(&[0u8; 32]); // compressorname
    entry.extend_from_slice(&u16(0x0018));
    entry.extend_from_slice(&u16(0xFFFF));
    for child in children {
        entry.extend(child);
    }
    let mut with_header = Vec::new();
    with_header.extend_from_slice(&u32(entry.len() as u32 + 8));
    with_header.extend_from_slice(coding);
    with_header.extend_from_slice(&entry);
    with_header
}

/// Audio sample entry (`coding`) with `channels`/`rate_hz` and child boxes.
fn audio_entry(coding: &[u8; 4], channels: u16, rate_hz: u32, children: &[Vec<u8>]) -> Vec<u8> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&[0; 6]); // reserved
    entry.extend_from_slice(&u16(1)); // data_reference_index
    entry.extend_from_slice(&[0; 8]); // reserved
    entry.extend_from_slice(&u16(channels));
    entry.extend_from_slice(&u16(16)); // samplesize
    entry.extend_from_slice(&u16(0)); // pre_defined
    entry.extend_from_slice(&u16(0)); // reserved
    entry.extend_from_slice(&u32(rate_hz << 16)); // samplerate 16.16
    for child in children {
        entry.extend(child);
    }
    let mut with_header = Vec::new();
    with_header.extend_from_slice(&u32(entry.len() as u32 + 8));
    with_header.extend_from_slice(coding);
    with_header.extend_from_slice(&entry);
    with_header
}

/// Bare sample entry used for `Other` kinds.
fn plain_entry(coding: &[u8; 4]) -> Vec<u8> {
    let mut with_header = Vec::new();
    with_header.extend_from_slice(&u32(8));
    with_header.extend_from_slice(coding);
    with_header
}

/// `stz2` compact sample sizes (§8.7.3.3).
fn stz2(field_size: u8, sizes: &[u32]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[0, 0, 0]); // reserved (u24)
    p.push(field_size);
    p.extend_from_slice(&u32(sizes.len() as u32));
    match field_size {
        4 => {
            for pair in sizes.chunks(2) {
                let hi = (pair[0] as u8 & 0x0F) << 4;
                let lo = pair.get(1).copied().unwrap_or(0) as u8 & 0x0F;
                p.push(hi | lo);
            }
        }
        8 => {
            for &s in sizes {
                p.push(s as u8);
            }
        }
        _ => {
            for &s in sizes {
                p.extend_from_slice(&u16(s as u16));
            }
        }
    }
    full(b"stz2", 0, 0, &p)
}

/// `mdhd` v1 with 64-bit times.
fn mdhd_v1(ts: u32, dur: u64) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u64(0));
    p.extend_from_slice(&u64(0));
    p.extend_from_slice(&u32(ts));
    p.extend_from_slice(&u64(dur));
    p.extend_from_slice(&u16(0x15C7));
    p.extend_from_slice(&u16(0));
    full(b"mdhd", 1, 0, &p)
}

/// `mdhd` with a packed language that does not decode to letters.
fn mdhd_bad_language(ts: u32, dur: u32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&u32(0));
    p.extend_from_slice(&u32(0));
    p.extend_from_slice(&u32(ts));
    p.extend_from_slice(&u32(dur));
    p.extend_from_slice(&u16(0xFFFF)); // letters 31,31,31 -> invalid
    p.extend_from_slice(&u16(0));
    full(b"mdhd", 0, 0, &p)
}

fn four_samples() -> ([u32; 4], [(u32, u32, u32); 1]) {
    ([3, 5, 3, 5], [(1, 2, 1)])
}

#[test]
fn reader_data_and_boxes_find() {
    let file = build_file(
        false,
        false,
        false,
        16,
        &[3, 5, 3, 5],
        &[(1, 2, 1)],
        Some(&[1, 3]),
    );
    let r = pith_mp4::Reader::new(&file);
    assert_eq!(r.data().len(), file.len());
    assert_eq!(r.remaining(), file.len());
    let moov = pith_mp4::Boxes::find(&file, 0..file.len(), *b"moov")
        .expect("find must not error")
        .expect("moov must exist");
    assert_eq!(moov.four, *b"moov");
    let none = pith_mp4::Boxes::find(&file, 0..file.len(), *b"zzzz").expect("find must not error");
    assert!(none.is_none());
}

#[test]
fn mdhd_v1_headers_are_read() {
    let (sizes, runs) = four_samples();
    let offsets = chunk_offsets(8, &sizes, &runs);
    let mut moov_body = Vec::new();
    moov_body.extend(mvhd());
    let mut mdia_body = Vec::new();
    mdia_body.extend(mdhd_v1(90_000, 180_000));
    mdia_body.extend(hdlr(b"vide"));
    mdia_body.extend(minf(&offsets, false, &sizes, &runs, Some(&[1, 3])));
    let mut trak_body = Vec::new();
    trak_body.extend(tkhd(1, 640, 360));
    trak_body.extend(bx(b"mdia", &mdia_body));
    moov_body.extend(bx(b"trak", &trak_body));
    let mut file = ftyp();
    file.extend(bx(b"moov", &moov_body));
    file.extend(mdat(&concat_samples(&sizes)));
    let mp4 = pith_mp4::demux(&file).expect("v1 headers must demux");
    assert_eq!(mp4.tracks[0].timescale, 90_000);
    assert_eq!(mp4.tracks[0].duration, 180_000);
    assert_eq!(mp4.tracks[0].language, Some(*b"eng"));
}

#[test]
fn mdhd_with_invalid_language_is_none() {
    let (sizes, runs) = four_samples();
    let offsets = chunk_offsets(8, &sizes, &runs);
    let mut moov_body = Vec::new();
    moov_body.extend(mvhd());
    let mut mdia_body = Vec::new();
    mdia_body.extend(mdhd_bad_language(90_000, 180_000));
    mdia_body.extend(hdlr(b"vide"));
    mdia_body.extend(minf(&offsets, false, &sizes, &runs, Some(&[1, 3])));
    let mut trak_body = Vec::new();
    trak_body.extend(tkhd(1, 640, 360));
    trak_body.extend(bx(b"mdia", &mdia_body));
    moov_body.extend(bx(b"trak", &trak_body));
    let mut file = ftyp();
    file.extend(bx(b"moov", &moov_body));
    file.extend(mdat(&concat_samples(&sizes)));
    let mp4 = pith_mp4::demux(&file).expect("bad-language fixture must still demux");
    assert_eq!(mp4.tracks[0].language, None);
}

#[test]
fn stz2_compact_sizes_are_expanded() {
    let sizes = [3u32; 4];
    let runs = [(1u32, 2u32, 1u32)];
    let offsets = chunk_offsets(8, &sizes, &runs);
    let mut stbl_body = Vec::new();
    stbl_body.extend(stsd_avc1(640, 360));
    stbl_body.extend(stts(&[(4, 45_000)]));
    stbl_body.extend(stsc(&runs));
    stbl_body.extend(stz2(4, &sizes));
    stbl_body.extend(stco(&offsets));
    let mut file = ftyp();
    file.extend(bx(b"moov", &video_trak(&stbl_body)));
    file.extend(mdat(&concat_samples(&sizes)));
    let mp4 = pith_mp4::demux(&file).expect("stz2 fixture must demux");
    let track = &mp4.tracks[0];
    assert_eq!(track.table.len(), 4);
    assert_eq!(track.table.uniform_size, 0);
    let samples: Vec<_> = track.samples().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(samples.iter().all(|s| s.size == 3));
    // stz2 with an invalid field size is a BadValue, not a panic.
    let mut bad = Vec::new();
    bad.extend(stsd_avc1(640, 360));
    bad.extend(stts(&[(4, 45_000)]));
    bad.extend(stsc(&runs));
    bad.extend(stz2(12, &sizes));
    bad.extend(stco(&offsets));
    let mut bad_file = ftyp();
    bad_file.extend(bx(b"moov", &video_trak(&bad)));
    bad_file.extend(mdat(&concat_samples(&sizes)));
    assert!(matches!(
        pith_mp4::demux(&bad_file),
        Err(pith_digest::Error::BadValue(_))
    ));
}

#[test]
fn stsd_entry_kinds_visual_audio_other() {
    let sizes = [4u32; 2];
    let runs = [(1u32, 2u32, 1u32)];
    let offsets = chunk_offsets(8, &sizes, &runs);
    let pasp = bx(b"pasp", &[0, 0, 0, 1, 0, 0, 0, 1]);
    let avc1 = visual_entry(b"avc1", 320, 240, &[bx(b"avcC", &avcc_payload()), pasp]);
    let no_avcc = visual_entry(b"avc3", 320, 240, &[]);
    let mp4a = audio_entry(b"mp4a", 2, 44_100, &[bx(b"esds", &[3, 0x19, 0, 0, 0])]);
    let text = plain_entry(b"text");
    let mut stbl_body = Vec::new();
    stbl_body.extend(stsd_with(&[avc1, no_avcc, mp4a, text]));
    stbl_body.extend(stts(&[(2, 45_000)]));
    stbl_body.extend(stsc(&runs));
    stbl_body.extend(stsz(&sizes));
    stbl_body.extend(stco(&offsets));
    let mut file = ftyp();
    file.extend(bx(b"moov", &video_trak(&stbl_body)));
    file.extend(mdat(&concat_samples(&sizes)));
    let mp4 = pith_mp4::demux(&file).expect("entry-kinds fixture must demux");
    let entries = &mp4.tracks[0].table.description.entries;
    assert_eq!(entries.len(), 4);
    match &entries[0] {
        pith_mp4::EntryKind::Visual {
            coding,
            width,
            height,
            avcc,
            ..
        } => {
            assert_eq!(coding, b"avc1");
            assert_eq!((*width, *height), (320, 240));
            assert_eq!(avcc.as_deref(), Some(&avcc_payload()[..]));
        }
        other => panic!("expected Visual, got {other:?}"),
    }
    // The second visual entry carries no avcC, so avcc() reports None.
    assert!(mp4.tracks[0].avcc().is_some());
    match &entries[1] {
        pith_mp4::EntryKind::Visual { avcc, .. } => assert!(avcc.is_none()),
        other => panic!("expected Visual, got {other:?}"),
    }
    match &entries[2] {
        pith_mp4::EntryKind::Audio {
            coding,
            channels,
            rate,
            esds,
        } => {
            assert_eq!(coding, b"mp4a");
            assert_eq!(*channels, 2);
            assert_eq!(*rate, 44_100);
            assert_eq!(esds.as_deref(), Some(&[3u8, 0x19, 0, 0, 0][..]));
        }
        other => panic!("expected Audio, got {other:?}"),
    }
    match &entries[3] {
        pith_mp4::EntryKind::Other { coding } => assert_eq!(coding, b"text"),
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn avcc_is_none_without_avc_record() {
    let sizes = [4u32; 2];
    let runs = [(1u32, 2u32, 1u32)];
    let offsets = chunk_offsets(8, &sizes, &runs);
    let mut stbl_body = Vec::new();
    stbl_body.extend(stsd_with(&[visual_entry(b"avc3", 320, 240, &[])]));
    stbl_body.extend(stts(&[(2, 45_000)]));
    stbl_body.extend(stsc(&runs));
    stbl_body.extend(stsz(&sizes));
    stbl_body.extend(stco(&offsets));
    let mut file = ftyp();
    file.extend(bx(b"moov", &video_trak(&stbl_body)));
    file.extend(mdat(&concat_samples(&sizes)));
    let mp4 = pith_mp4::demux(&file).expect("avc3 fixture must demux");
    assert!(mp4.tracks[0].avcc().is_none());
}

#[test]
fn two_moov_boxes_are_rejected() {
    let (sizes, runs) = four_samples();
    let offsets = chunk_offsets(8, &sizes, &runs);
    let mut file = ftyp();
    file.extend(moov(&offsets, false, &sizes, &runs, None));
    file.extend(bx(b"moov", &[]));
    match pith_mp4::demux(&file) {
        Err(pith_digest::Error::BadValue(what)) => {
            assert_eq!(what, "more than one moov box")
        }
        other => panic!("expected BadValue, got {other:?}"),
    }
}

#[test]
fn mvex_in_moov_is_unsupported() {
    let file = file_from_moov(&[mvhd(), bx(b"mvex", &[])]);
    match pith_mp4::demux(&file) {
        Err(pith_digest::Error::Unsupported(what)) => {
            assert_eq!(what, "fragmented mp4 (mvex)")
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn stss_past_sample_count_is_rejected() {
    let (sizes, runs) = four_samples();
    let offsets = chunk_offsets(8, &sizes, &runs);
    let mut stbl_body = Vec::new();
    stbl_body.extend(stsd_avc1(640, 360));
    stbl_body.extend(stts(&[(4, 45_000)]));
    stbl_body.extend(stsc(&runs));
    stbl_body.extend(stsz(&sizes));
    stbl_body.extend(stco(&offsets));
    stbl_body.extend(stss(&[5])); // sample_count is 4
    let mut file = ftyp();
    file.extend(bx(b"moov", &video_trak(&stbl_body)));
    file.extend(mdat(&concat_samples(&sizes)));
    match pith_mp4::demux(&file) {
        Err(pith_digest::Error::BadValue(what)) => {
            assert_eq!(what, "stss index past sample count")
        }
        other => panic!("expected BadValue, got {other:?}"),
    }
}

#[test]
fn stsc_with_no_runs_fails_upfront_validation() {
    let sizes = [3u32; 2];
    let mut stbl_body = Vec::new();
    stbl_body.extend(stsd_avc1(640, 360));
    stbl_body.extend(stts(&[(2, 45_000)]));
    stbl_body.extend(stsc(&[])); // no runs at all
    stbl_body.extend(stsz(&sizes));
    stbl_body.extend(stco(&[100]));
    let mut file = ftyp();
    file.extend(bx(b"moov", &video_trak(&stbl_body)));
    file.extend(mdat(&concat_samples(&sizes)));
    // demux validates every declared sample range up front, so the empty
    // run list surfaces as a top-level error, not mid-iteration.
    match pith_mp4::demux(&file) {
        Err(pith_digest::Error::BadValue(what)) => {
            assert_eq!(what, "stsc has no run covering every chunk")
        }
        other => panic!("expected BadValue, got {other:?}"),
    }
}

#[test]
fn zero_sample_track_is_empty() {
    let mut stbl_body = Vec::new();
    stbl_body.extend(stsd_avc1(640, 360));
    stbl_body.extend(stts(&[]));
    stbl_body.extend(stsc(&[]));
    stbl_body.extend(stsz(&[]));
    stbl_body.extend(stco(&[]));
    let mut file = ftyp();
    file.extend(bx(b"moov", &video_trak(&stbl_body)));
    file.extend(mdat(&[]));
    let mp4 = pith_mp4::demux(&file).expect("empty track must demux");
    let table = &mp4.tracks[0].table;
    assert!(table.is_empty());
    assert_eq!(table.len(), 0);
    assert_eq!(table.samples().next(), None);
    assert!(table.is_keyframe(0));
}
