// SPDX-License-Identifier: MIT
// Copyright (c) 2026 pith-hash

// Package pithmp4 provides Go bindings for the pith-mp4 Rust cdylib:
// ISO-BMFF demuxing into the canonical record walk.
//
// The single Rust core (built by `cargo build --release`) is loaded at
// runtime; the package carries zero module dependencies. On unix the
// cdylib is opened with dlopen through cgo, on Windows with
// LoadLibrary through the standard syscall package — both resolve the
// library through the same discovery chain, so `go build ./... &&
// go test ./...` works unchanged on every OS the CD matrix builds.
//
// Discovery order (the suite's cdylib convention):
//
//  1. PITH_CDYLIB — an explicit cdylib file path;
//  2. PITH_CDYLIB_DIR — a directory scanned for the cdylib names (the
//     CD pipeline points this at target/release);
//  3. <repo root>/target/release — the repository working-tree layout,
//     anchored at this package's source directory, so a source
//     checkout runs against a local cargo build unconfigured.
//
// The FFI surface is one demux operation plus one free:
// pith_mp4_demux demuxes a whole ISO-BMFF file into the canonical
// record walk the reference.json vectors are checked against (the
// demuxed shape serialized deterministically, field by field,
// big-endian, exactly as the crate's ffi module documents it), and
// pith_mp4_free releases the handed-out buffer.
package pithmp4

import (
	"encoding/binary"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"unsafe"
)

// Status codes returned by the cdylib's C ABI.
const (
	// StatusOK: success.
	StatusOK int32 = 0
	// StatusInvalid: a caller argument is invalid (a null pointer).
	StatusInvalid int32 = -1
	// StatusRejected: the core demuxer refused the input (no ftyp,
	// unrecognised brands, missing moov, fragmented file, truncated
	// stream).
	StatusRejected int32 = -2
)

// Refusal kinds reported through FfiError.Kind on StatusRejected — the
// stable names reference.json's errors record as strings.
const (
	// ErrBadValue: Error::BadValue ("BadValue").
	ErrBadValue int32 = 1
	// ErrInvalidMagic: Error::InvalidMagic ("InvalidMagic").
	ErrInvalidMagic int32 = 2
	// ErrTooLarge: Error::TooLarge ("TooLarge").
	ErrTooLarge int32 = 3
	// ErrTruncated: Error::Truncated ("Truncated").
	ErrTruncated int32 = 4
	// ErrUnsupported: Error::Unsupported ("Unsupported").
	ErrUnsupported int32 = 5
)

// stsd entry kinds, the codes the canonical walk carries.
const (
	// EntryVisual: a visual sample entry.
	EntryVisual uint8 = 0
	// EntryAudio: an audio sample entry.
	EntryAudio uint8 = 1
	// EntryOther: neither visual nor audio.
	EntryOther uint8 = 2
)

// ErrKindNames maps a refusal kind code to the stable name
// reference.json records.
var ErrKindNames = map[int32]string{
	ErrBadValue:     "BadValue",
	ErrInvalidMagic: "InvalidMagic",
	ErrTooLarge:     "TooLarge",
	ErrTruncated:    "Truncated",
	ErrUnsupported:  "Unsupported",
}

// cdylibNames are the file names cargo may drop into the build
// directory, per platform (windows / linux / macOS).
var cdylibNames = []string{"pith_mp4.dll", "libpith_mp4.so", "libpith_mp4.dylib"}

// emptyAnchor backs zero-length inputs so they reach the demuxer as a
// valid pointer with length zero instead of a null pointer.
var emptyAnchor byte

// FfiError reports a non-zero status code from the cdylib.
type FfiError struct {
	// Op is the FFI operation name.
	Op string
	// Status is the raw status code the FFI returned.
	Status int32
	// Kind is the refusal kind code (0 unless Status is
	// StatusRejected); one of the Err* constants.
	Kind int32
}

func (e *FfiError) Error() string {
	kind := "unknown failure"
	switch e.Status {
	case StatusInvalid:
		kind = "invalid argument"
	case StatusRejected:
		kind = "input rejected"
	}
	if e.Kind != 0 {
		if name, ok := ErrKindNames[e.Kind]; ok {
			kind = fmt.Sprintf("%s (%s)", kind, name)
		} else {
			kind = fmt.Sprintf("%s (kind %d)", kind, e.Kind)
		}
	}
	return fmt.Sprintf("%s failed: %s (status %d)", e.Op, kind, e.Status)
}

// FindCdylib locates the cdylib through the suite's discovery chain.
func FindCdylib() (string, error) {
	if p := os.Getenv("PITH_CDYLIB"); p != "" {
		if st, err := os.Stat(p); err == nil && st.Mode().IsRegular() {
			return filepath.Abs(p)
		}
	}
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		return "", fmt.Errorf("pithmp4: cannot locate the package source directory")
	}
	pkgDir := filepath.Dir(thisFile)
	repoRoot := filepath.Dir(filepath.Dir(pkgDir)) // sdk/go -> sdk -> repo root

	var dirs []string
	if env := os.Getenv("PITH_CDYLIB_DIR"); env != "" {
		dirs = append(dirs, env)
		if !filepath.IsAbs(env) {
			dirs = append(dirs, filepath.Join(repoRoot, env))
		}
	}
	dirs = append(dirs, filepath.Join(repoRoot, "target", "release"))
	for _, dir := range dirs {
		for _, name := range cdylibNames {
			p := filepath.Join(dir, name)
			if st, err := os.Stat(p); err == nil && st.Mode().IsRegular() {
				return p, nil
			}
		}
	}
	return "", fmt.Errorf(
		"pithmp4: no cdylib found (searched PITH_CDYLIB, PITH_CDYLIB_DIR and <repo>/target/release); run `cargo build --release` first",
	)
}

// locate resolves the cdylib path once per process.
var locate = sync.OnceValues(FindCdylib)

// Sample is one sample record of the canonical walk.
type Sample struct {
	// Offset is the absolute byte offset of the sample in the file.
	Offset uint64
	// Size is the sample size in bytes.
	Size uint32
	// Decoding is the decoding time in the track timescale.
	Decoding uint64
	// Presentation is the presentation time in the track timescale
	// (may precede decoding with edit lists / B-frames).
	Presentation int64
	// Duration is the sample duration in the track timescale.
	Duration uint32
	// Keyframe reports whether the sample is a sync sample.
	Keyframe bool
}

// Entry is one stsd sample entry of the canonical walk.
type Entry struct {
	// Kind is "Visual", "Audio" or "Other".
	Kind string
	// Coding is the four-character coding (avc1, mp4a, ...).
	Coding string
	// Width is the visual-only coded width.
	Width uint32
	// Height is the visual-only coded height.
	Height uint32
	// AvccHex is the visual-only avcC payload, hex-encoded ("" when
	// absent).
	AvccHex string
	// Channels is the audio-only channel count.
	Channels uint32
	// Rate is the audio-only sample rate in Hz.
	Rate uint32
	// EsdsHex is the esds payload, hex-encoded ("" when absent).
	EsdsHex string
}

// Track is one demuxed track of the canonical walk.
type Track struct {
	// ID is the track id from tkhd.
	ID uint32
	// Timescale is the media timescale from mdhd.
	Timescale uint32
	// Duration is the track duration in the media timescale.
	Duration uint64
	// Language is the ISO-639-2/T language code ("" when absent).
	Language string
	// Handler is the handler type (vide, soun, ...).
	Handler string
	// Width is the visual track width from tkhd (0 for non-visual).
	Width uint32
	// Height is the visual track height from tkhd (0 for non-visual).
	Height uint32
	// Samples are all sample records, in sample order.
	Samples []Sample
	// Entries are all stsd sample entries.
	Entries []Entry
}

// Demuxed is a demuxed ISO-BMFF file, re-expressed from the canonical
// walk.
type Demuxed struct {
	// MajorBrand is the major brand of the ftyp.
	MajorBrand string
	// CompatibleBrands are the compatible brands of the ftyp.
	CompatibleBrands []string
	// Timescale is the movie timescale from mvhd.
	Timescale uint32
	// Duration is the movie duration in the movie timescale.
	Duration uint64
	// Tracks are the demuxed tracks, in file order.
	Tracks []Track
	// Raw is the canonical byte stream the walk is parsed from.
	Raw []byte
}

// DemuxCanonical demuxes a complete ISO-BMFF file into the canonical
// record walk the reference.json vectors are checked against. The
// returned slice is a Go copy; the handed-out cdylib buffer is
// released before returning.
func DemuxCanonical(data []byte) ([]byte, error) {
	libPath, err := locate()
	if err != nil {
		return nil, err
	}
	var out *byte
	var outLen uintptr
	var kind int32
	var dataPtr *byte
	if len(data) > 0 {
		dataPtr = &data[0]
	} else {
		// An empty input is a zero-length slice, not a null pointer:
		// hand the C ABI a valid anchor (the ctypes and koffi bindings
		// do the same), so the demuxer — not the argument check —
		// classifies it.
		dataPtr = &emptyAnchor
	}
	status, err := ffiDemux(libPath, dataPtr, len(data), &out, &outLen, &kind)
	if err != nil {
		return nil, err
	}
	if status != StatusOK {
		return nil, &FfiError{Op: "pith_mp4_demux", Status: status, Kind: kind}
	}
	buf := make([]byte, outLen)
	copy(buf, unsafe.Slice(out, outLen))
	ffiFree(libPath, out, outLen)
	return buf, nil
}

// reader is a sequential big-endian reader over the canonical walk.
type reader struct {
	raw []byte
	pos int
}

func (r *reader) take(n int) ([]byte, error) {
	if r.pos+n > len(r.raw) {
		return nil, fmt.Errorf("pithmp4: canonical walk is truncated")
	}
	chunk := r.raw[r.pos : r.pos+n]
	r.pos += n
	return chunk, nil
}

func (r *reader) u8() (uint8, error) {
	b, err := r.take(1)
	if err != nil {
		return 0, err
	}
	return b[0], nil
}

func (r *reader) u32() (uint32, error) {
	b, err := r.take(4)
	if err != nil {
		return 0, err
	}
	return binary.BigEndian.Uint32(b), nil
}

func (r *reader) u64() (uint64, error) {
	b, err := r.take(8)
	if err != nil {
		return 0, err
	}
	return binary.BigEndian.Uint64(b), nil
}

func (r *reader) i64() (int64, error) {
	v, err := r.u64()
	return int64(v), err
}

func (r *reader) four() (string, error) {
	b, err := r.take(4)
	if err != nil {
		return "", err
	}
	return string(b), nil
}

// hexOrNull hex-encodes a payload, mapping absence to "" (the walk
// carries absent blobs as zero-length; reference.json records null).
func hexOrNull(b []byte) string {
	if len(b) == 0 {
		return ""
	}
	return fmt.Sprintf("%x", b)
}

func parseEntry(r *reader) (Entry, error) {
	kindCode, err := r.u8()
	if err != nil {
		return Entry{}, err
	}
	coding, err := r.four()
	if err != nil {
		return Entry{}, err
	}
	switch kindCode {
	case EntryVisual:
		width, err := r.u32()
		if err != nil {
			return Entry{}, err
		}
		height, err := r.u32()
		if err != nil {
			return Entry{}, err
		}
		avcc, err := r.take(int(be32len(r)))
		if err != nil {
			return Entry{}, err
		}
		esds, err := r.take(int(be32len(r)))
		if err != nil {
			return Entry{}, err
		}
		return Entry{
			Kind: "Visual", Coding: coding, Width: width, Height: height,
			AvccHex: hexOrNull(avcc), EsdsHex: hexOrNull(esds),
		}, nil
	case EntryAudio:
		channels, err := r.u32()
		if err != nil {
			return Entry{}, err
		}
		rate, err := r.u32()
		if err != nil {
			return Entry{}, err
		}
		esds, err := r.take(int(be32len(r)))
		if err != nil {
			return Entry{}, err
		}
		return Entry{
			Kind: "Audio", Coding: coding, Channels: channels, Rate: rate,
			EsdsHex: hexOrNull(esds),
		}, nil
	default:
		return Entry{Kind: "Other", Coding: coding}, nil
	}
}

// be32len reads a u32 length prefix (kept separate so error paths stay
// readable).
func be32len(r *reader) uint32 {
	v, err := r.u32()
	if err != nil {
		return 0
	}
	return v
}

func parseTrack(r *reader) (Track, error) {
	id, err := r.u32()
	if err != nil {
		return Track{}, err
	}
	timescale, err := r.u32()
	if err != nil {
		return Track{}, err
	}
	duration, err := r.u64()
	if err != nil {
		return Track{}, err
	}
	language := ""
	present, err := r.u8()
	if err != nil {
		return Track{}, err
	}
	if present == 1 {
		code, err := r.take(3)
		if err != nil {
			return Track{}, err
		}
		language = string(code)
	} else if _, err := r.take(3); err != nil {
		return Track{}, err
	}
	handler, err := r.four()
	if err != nil {
		return Track{}, err
	}
	width, err := r.u32()
	if err != nil {
		return Track{}, err
	}
	height, err := r.u32()
	if err != nil {
		return Track{}, err
	}
	sampleCount, err := r.u32()
	if err != nil {
		return Track{}, err
	}
	samples := make([]Sample, 0, sampleCount)
	for i := uint32(0); i < sampleCount; i++ {
		offset, err := r.u64()
		if err != nil {
			return Track{}, err
		}
		size, err := r.u32()
		if err != nil {
			return Track{}, err
		}
		decoding, err := r.u64()
		if err != nil {
			return Track{}, err
		}
		presentation, err := r.i64()
		if err != nil {
			return Track{}, err
		}
		sampleDuration, err := r.u32()
		if err != nil {
			return Track{}, err
		}
		key, err := r.u8()
		if err != nil {
			return Track{}, err
		}
		samples = append(samples, Sample{
			Offset: offset, Size: size, Decoding: decoding,
			Presentation: presentation, Duration: sampleDuration,
			Keyframe: key == 1,
		})
	}
	entryCount, err := r.u32()
	if err != nil {
		return Track{}, err
	}
	entries := make([]Entry, 0, entryCount)
	for i := uint32(0); i < entryCount; i++ {
		entry, err := parseEntry(r)
		if err != nil {
			return Track{}, err
		}
		entries = append(entries, entry)
	}
	return Track{
		ID: id, Timescale: timescale, Duration: duration, Language: language,
		Handler: handler, Width: width, Height: height,
		Samples: samples, Entries: entries,
	}, nil
}

// ParseCanonical re-expresses the canonical record walk as a Demuxed.
func ParseCanonical(raw []byte) (*Demuxed, error) {
	r := &reader{raw: raw}
	majorBrand, err := r.four()
	if err != nil {
		return nil, err
	}
	brandCount, err := r.u32()
	if err != nil {
		return nil, err
	}
	brands := make([]string, 0, brandCount)
	for i := uint32(0); i < brandCount; i++ {
		brand, err := r.four()
		if err != nil {
			return nil, err
		}
		brands = append(brands, brand)
	}
	timescale, err := r.u32()
	if err != nil {
		return nil, err
	}
	duration, err := r.u64()
	if err != nil {
		return nil, err
	}
	trackCount, err := r.u32()
	if err != nil {
		return nil, err
	}
	tracks := make([]Track, 0, trackCount)
	for i := uint32(0); i < trackCount; i++ {
		track, err := parseTrack(r)
		if err != nil {
			return nil, err
		}
		tracks = append(tracks, track)
	}
	return &Demuxed{
		MajorBrand:       majorBrand,
		CompatibleBrands: brands,
		Timescale:        timescale,
		Duration:         duration,
		Tracks:           tracks,
		Raw:              raw,
	}, nil
}
