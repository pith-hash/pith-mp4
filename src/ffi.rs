//! The C ABI surface of `pith-mp4`: the entry points the Python
//! (ctypes), Node (koffi) and Go (cgo) SDKs bind through.
//!
//! The suite's FFI convention, defined by the pilot cdylibs and
//! mirrored by every `pith-*` cdylib:
//!
//! * one flat set of `#[unsafe(no_mangle)] pub unsafe extern "C"`
//!   functions — raw pointers plus lengths, no structs across the
//!   boundary;
//! * every function returns a status code (see the constants below),
//!   never a `Result`, never a panic: a `panic = "abort"` cdylib must
//!   not be reachable from a foreign caller;
//! * an operation either hands ownership to the caller (and ships a
//!   matching `_free` — [`pith_mp4_free`] here) or writes into
//!   caller-provided out-parameters;
//! * the `unsafe` allowance is confined to this module; every core
//!   module stays unsafe-free behind the crate-root `#![deny]`.
//!
//! # Canonical wire format
//!
//! `reference.json` pins facts only (brands, timescales, per-sample
//! records, `stsd` entries), so [`pith_mp4_demux`] hands the caller
//! the **canonical record walk**: the demuxed shape serialized
//! deterministically, field by field, all integers big-endian:
//!
//! ```text
//! major_brand            4 bytes
//! compatible_brands      u32 count, then 4 bytes each
//! timescale              u32
//! duration               u64
//! track_count            u32
//!   per track:
//!     id                 u32
//!     timescale          u32
//!     duration           u64
//!     language           u8 present flag (1/0), then 3 bytes
//!     handler            4 bytes
//!     width              u32
//!     height             u32
//!     sample_count       u32
//!     per sample:
//!       offset           u64
//!       size             u32
//!       decoding         u64
//!       presentation     u64 (two's complement of the i64 PTS)
//!       duration         u32
//!       keyframe         u8 (1/0)
//!     stsd_entry_count   u32
//!     per entry:
//!       kind             u8 (0 = Visual, 1 = Audio, 2 = Other)
//!       coding           4 bytes
//!       Visual: width u32, height u32,
//!               avcc u32 len + bytes, esds u32 len + bytes
//!       Audio:  channels u32, rate u32,
//!               esds u32 len + bytes
//!       Other:  (nothing beyond the coding)
//! ```
//!
//! On a refusal the `err` out-parameter carries one of the
//! `PITH_ERR_*` kind codes — the stable variant names `reference.json`
//! `errors` records as numbers, so an SDK test can assert the exact
//! refusal kind.

#![allow(unsafe_code)]

use alloc::vec::Vec;

use crate::{EntryKind, demux};
use pith_digest::Error;

/// Status: success.
pub const PITH_OK: i32 = 0;
/// Status: a caller argument is invalid — a null pointer.
pub const PITH_E_INVALID: i32 = -1;
/// Status: the core demuxer refused the input (no `ftyp`, unrecognised
/// brands, missing `moov`, fragmented file, or a truncated stream).
pub const PITH_E_REJECTED: i32 = -2;

/// Refusal kind: `Error::BadValue` (`"BadValue"` in reference.json).
pub const PITH_ERR_BAD_VALUE: i32 = 1;
/// Refusal kind: `Error::InvalidMagic` (`"InvalidMagic"`).
pub const PITH_ERR_INVALID_MAGIC: i32 = 2;
/// Refusal kind: `Error::TooLarge` (`"TooLarge"`).
pub const PITH_ERR_TOO_LARGE: i32 = 3;
/// Refusal kind: `Error::Truncated` (`"Truncated"`).
pub const PITH_ERR_TRUNCATED: i32 = 4;
/// Refusal kind: `Error::Unsupported` (`"Unsupported"`).
pub const PITH_ERR_UNSUPPORTED: i32 = 5;

/// `stsd` entry kind code: a visual sample entry.
pub const PITH_ENTRY_VISUAL: u8 = 0;
/// `stsd` entry kind code: an audio sample entry.
pub const PITH_ENTRY_AUDIO: u8 = 1;
/// `stsd` entry kind code: neither visual nor audio.
pub const PITH_ENTRY_OTHER: u8 = 2;

/// Maps a demuxer error to its stable kind code (alphabetical over the
/// variant names reference.json records).
fn err_kind(e: &Error) -> i32 {
    match e {
        Error::BadValue(_) => PITH_ERR_BAD_VALUE,
        Error::InvalidMagic { .. } => PITH_ERR_INVALID_MAGIC,
        Error::TooLarge { .. } => PITH_ERR_TOO_LARGE,
        Error::Truncated { .. } => PITH_ERR_TRUNCATED,
        Error::Unsupported(_) => PITH_ERR_UNSUPPORTED,
    }
}

/// Appends one big-endian `u32` field.
fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Appends one big-endian `u64` field.
fn push_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Appends `u32 len + bytes`, the length-prefixed blob shape.
fn push_blob(out: &mut Vec<u8>, bytes: &[u8]) {
    push_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

/// Serializes one demuxed file into the canonical record walk.
fn serialize(mp4: &crate::Mp4) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&mp4.major_brand);
    push_u32(&mut out, mp4.compatible_brands.len() as u32);
    for brand in &mp4.compatible_brands {
        out.extend_from_slice(brand);
    }
    push_u32(&mut out, mp4.timescale);
    push_u64(&mut out, mp4.duration);
    push_u32(&mut out, mp4.tracks.len() as u32);
    for track in &mp4.tracks {
        push_u32(&mut out, track.id);
        push_u32(&mut out, track.timescale);
        push_u64(&mut out, track.duration);
        match track.language {
            Some(code) => {
                out.push(1);
                out.extend_from_slice(&code);
            }
            None => out.extend_from_slice(&[0, 0, 0, 0]),
        }
        out.extend_from_slice(&track.handler);
        push_u32(&mut out, track.width);
        push_u32(&mut out, track.height);

        let samples: Vec<_> = track
            .samples()
            .collect::<Result<Vec<_>, _>>()
            .expect("demux validated every sample range already");
        push_u32(&mut out, samples.len() as u32);
        for s in &samples {
            push_u64(&mut out, s.offset);
            push_u32(&mut out, s.size);
            push_u64(&mut out, s.decoding);
            push_u64(&mut out, s.presentation as u64);
            push_u32(&mut out, s.duration);
            out.push(u8::from(s.keyframe));
        }

        let entries = &track.table.description.entries;
        push_u32(&mut out, entries.len() as u32);
        for entry in entries {
            match entry {
                EntryKind::Visual {
                    coding,
                    width,
                    height,
                    avcc,
                    esds,
                } => {
                    out.push(PITH_ENTRY_VISUAL);
                    out.extend_from_slice(coding);
                    push_u32(&mut out, u32::from(*width));
                    push_u32(&mut out, u32::from(*height));
                    push_blob(&mut out, avcc.as_deref().unwrap_or(&[]));
                    push_blob(&mut out, esds.as_deref().unwrap_or(&[]));
                }
                EntryKind::Audio {
                    coding,
                    channels,
                    rate,
                    esds,
                } => {
                    out.push(PITH_ENTRY_AUDIO);
                    out.extend_from_slice(coding);
                    push_u32(&mut out, u32::from(*channels));
                    push_u32(&mut out, *rate);
                    push_blob(&mut out, esds.as_deref().unwrap_or(&[]));
                }
                EntryKind::Other { coding } => {
                    out.push(PITH_ENTRY_OTHER);
                    out.extend_from_slice(coding);
                }
            }
        }
    }
    out
}

/// Hands a serialized canonical walk to the caller: the exact-length
/// buffer goes out as an owned boxed slice; [`pith_mp4_free`]
/// reconstructs it from the same length to release it.
unsafe fn hand_out(canonical: Vec<u8>, out: *mut *mut u8, out_len: *mut usize) {
    let len = canonical.len();
    let ptr = alloc::boxed::Box::into_raw(canonical.into_boxed_slice());
    unsafe {
        *out = ptr.cast::<u8>();
        *out_len = len;
    }
}

/// Writes the refusal shape through the caller's out-parameters: a
/// null buffer, zero length and the stable kind code.
unsafe fn refuse(out: *mut *mut u8, out_len: *mut usize, err: *mut i32, kind: i32) {
    unsafe {
        *out = core::ptr::null_mut();
        *out_len = 0;
        *err = kind;
    }
}

/// Demuxes an ISO-BMFF file into the canonical record walk the SDK
/// vectors are checked against.
///
/// `data` points at `len` bytes of the complete file. On success the
/// function allocates a buffer, writes its address through `out`, its
/// length through `out_len`, a zero kind through `err`, and returns
/// [`PITH_OK`]; the caller owns the buffer and must release it with
/// [`pith_mp4_free`], passing back the same pointer *and* length. The
/// buffer layout is the canonical wire format documented on this
/// module.
///
/// On a refusal the function returns [`PITH_E_REJECTED`], writes zero
/// through `out`, and stores one `PITH_ERR_*` kind code through `err`.
///
/// # Safety
///
/// `data` must point to `len` readable bytes; `out` and `out_len` to
/// one writable pointer/`usize` each; `err` to one writable `i32`. All
/// must stay valid for the duration of the call; the function retains
/// nothing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pith_mp4_demux(
    data: *const u8,
    len: usize,
    out: *mut *mut u8,
    out_len: *mut usize,
    err: *mut i32,
) -> i32 {
    if data.is_null() || out.is_null() || out_len.is_null() || err.is_null() {
        return PITH_E_INVALID;
    }
    let bytes = unsafe { core::slice::from_raw_parts(data, len) };
    match demux(bytes) {
        Ok(mp4) => {
            unsafe {
                hand_out(serialize(&mp4), out, out_len);
                *err = PITH_OK;
            }
            PITH_OK
        }
        Err(e) => {
            unsafe { refuse(out, out_len, err, err_kind(&e)) };
            PITH_E_REJECTED
        }
    }
}

/// Releases a buffer handed out by [`pith_mp4_demux`].
///
/// # Safety
///
/// `ptr` must be a pointer returned by [`pith_mp4_demux`] with the
/// `out_len` value that came back with it, and must not have been
/// released (or otherwise freed) before. Null is accepted and
/// ignored, so callers can free unconditionally on the error path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pith_mp4_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    let slice = unsafe { core::slice::from_raw_parts_mut(ptr, len) };
    drop(unsafe { alloc::boxed::Box::from_raw(slice) });
}

#[cfg(test)]
mod tests {
    use super::{
        PITH_E_INVALID, PITH_E_REJECTED, PITH_ERR_BAD_VALUE, PITH_ERR_INVALID_MAGIC,
        PITH_ERR_TOO_LARGE, PITH_ERR_TRUNCATED, PITH_ERR_UNSUPPORTED, PITH_OK, err_kind,
        pith_mp4_demux, pith_mp4_free, serialize,
    };
    use crate::demux;
    use pith_digest::{Error, sha256};

    /// The committed minimal fixture, demuxed end-to-end through the
    /// raw FFI: status OK, the record walk carries the brands and a
    /// pinned sha256, and the buffer round-trips through
    /// `pith_mp4_free`.
    #[test]
    fn ffi_demux_reproduces_the_record_walk() {
        let path = format!(
            "{}/tests/fixtures/minimal-4-samples.mp4",
            env!("CARGO_MANIFEST_DIR")
        );
        let input = std::fs::read(&path).expect("fixture");
        let expected = serialize(&demux(&input).expect("demux"));

        let mut out: *mut u8 = core::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err: i32 = -99;
        let status = unsafe {
            pith_mp4_demux(
                input.as_ptr(),
                input.len(),
                &mut out,
                &mut out_len,
                &mut err,
            )
        };
        assert_eq!(status, PITH_OK);
        assert_eq!(err, PITH_OK);
        assert_eq!(out_len, expected.len());
        let handed_back = unsafe { core::slice::from_raw_parts(out, out_len) };
        assert_eq!(handed_back, expected.as_slice());
        // The walk opens with the major brand then the brand count.
        assert_eq!(&handed_back[..8], b"isom\x00\x00\x00\x03");
        // Pinned drift guard: the whole record walk digests to a value
        // the three SDK harnesses pin too.
        let digest = sha256(handed_back).expect("sha256");
        assert_eq!(
            digest.as_bytes(),
            &hex_bytes("be4b61fc669c451eb943875a47f2971daf92defbe20e58d9551ab29f76f89d24")[..]
        );
        unsafe { pith_mp4_free(out, out_len) };
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// Every committed error fixture is refused with its recorded
    /// kind; null pointers are [`PITH_E_INVALID`]; a null buffer is a
    /// legal free.
    #[test]
    fn ffi_refusals() {
        let mut out: *mut u8 = core::ptr::null_mut();
        let mut out_len: usize = 0;
        let mut err: i32 = -99;
        let null_data =
            unsafe { pith_mp4_demux(core::ptr::null(), 0, &mut out, &mut out_len, &mut err) };
        assert_eq!(null_data, PITH_E_INVALID);

        let stream = [0u8; 16];
        let null_err = unsafe {
            pith_mp4_demux(
                stream.as_ptr(),
                stream.len(),
                &mut out,
                &mut out_len,
                core::ptr::null_mut(),
            )
        };
        assert_eq!(null_err, PITH_E_INVALID);

        // fragmented-moof: recorded kind "Unsupported".
        let path = format!(
            "{}/tests/fixtures/fragmented-moof.mp4",
            env!("CARGO_MANIFEST_DIR")
        );
        let fragmented = std::fs::read(&path).expect("fixture");
        let refused = unsafe {
            pith_mp4_demux(
                fragmented.as_ptr(),
                fragmented.len(),
                &mut out,
                &mut out_len,
                &mut err,
            )
        };
        assert_eq!(refused, PITH_E_REJECTED);
        assert_eq!(err, PITH_ERR_UNSUPPORTED);
        assert!(out.is_null() && out_len == 0);

        unsafe { pith_mp4_free(core::ptr::null_mut(), 0) };
    }

    /// A truncated prefix of the minimal fixture is refused
    /// (`InvalidMagic` or `Truncated` — never a crash).
    #[test]
    fn ffi_truncated_prefixes_are_refused() {
        let path = format!(
            "{}/tests/fixtures/minimal-4-samples.mp4",
            env!("CARGO_MANIFEST_DIR")
        );
        let input = std::fs::read(&path).expect("fixture");
        for cut in [0usize, 3, 17, 100, input.len() - 1] {
            let mut out: *mut u8 = core::ptr::null_mut();
            let mut out_len: usize = 0;
            let mut err: i32 = -99;
            let status =
                unsafe { pith_mp4_demux(input.as_ptr(), cut, &mut out, &mut out_len, &mut err) };
            assert_eq!(status, PITH_E_REJECTED, "cut {cut}");
            assert!(
                err == PITH_ERR_INVALID_MAGIC || err == 4,
                "cut {cut} err {err}"
            );
            assert!(out.is_null());
        }
    }

    /// Every decoder error maps to its stable kind code.
    #[test]
    fn err_kinds_map_one_to_one() {
        assert_eq!(err_kind(&Error::BadValue("x")), PITH_ERR_BAD_VALUE);
        assert_eq!(
            err_kind(&Error::InvalidMagic { what: "x" }),
            PITH_ERR_INVALID_MAGIC
        );
        assert_eq!(err_kind(&Error::too_large("x", 0)), PITH_ERR_TOO_LARGE);
        assert_eq!(err_kind(&Error::truncated("x", 1, 0)), PITH_ERR_TRUNCATED);
        assert_eq!(err_kind(&Error::Unsupported("x")), PITH_ERR_UNSUPPORTED);
    }
}
