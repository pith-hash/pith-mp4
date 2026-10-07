// SPDX-License-Identifier: MIT
// Copyright (c) 2026 pith-hash

package pithmp4

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// repoRoot resolves the repository root relative to this package
// (sdk/go -> sdk -> repo root), the anchor for reference.json and the
// committed fixtures.
func repoRoot(t *testing.T) string {
	t.Helper()
	root, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	if st, err := os.Stat(filepath.Join(root, "reference.json")); err != nil || st.IsDir() {
		t.Fatalf("reference.json not found at %s", root)
	}
	return root
}

// fixtureBytes reads one committed fixture file.
func fixtureBytes(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(repoRoot(t), "tests", "fixtures", name+".mp4"))
	if err != nil {
		t.Fatal(err)
	}
	return data
}

// mp4Sample mirrors one per-sample record of a reference vector.
type mp4Sample struct {
	Offset       uint64 `json:"offset"`
	Size         uint32 `json:"size"`
	Decoding     uint64 `json:"decoding"`
	Presentation uint64 `json:"presentation"`
	Duration     uint32 `json:"duration"`
	Keyframe     bool   `json:"keyframe"`
}

// mp4Entry mirrors one stsd entry of a reference vector.
type mp4Entry struct {
	Kind     string  `json:"kind"`
	Coding   string  `json:"coding"`
	Width    *uint32 `json:"width"`
	Height   *uint32 `json:"height"`
	AvccHex  *string `json:"avcc_hex"`
	Channels *uint32 `json:"channels"`
	Rate     *uint32 `json:"rate"`
	EsdsHex  *string `json:"esds_hex"`
}

// mp4Track mirrors one track of a reference vector.
type mp4Track struct {
	ID             uint64      `json:"id"`
	Timescale      uint64      `json:"timescale"`
	Duration       uint64      `json:"duration"`
	Language       *string     `json:"language"`
	Handler        string      `json:"handler"`
	Width          uint32      `json:"width"`
	Height         uint32      `json:"height"`
	SampleCount    uint64      `json:"sample_count"`
	Samples        []mp4Sample `json:"samples"`
	StsdEntryCount uint64      `json:"stsd_entry_count"`
	StsdEntries    []mp4Entry  `json:"stsd_entries"`
	AvccHex        *string     `json:"avcc_hex"`
}

// mp4Vector mirrors one success vector of reference.json.
type mp4Vector struct {
	Name             string     `json:"name"`
	FileSha256       string     `json:"file_sha256"`
	FileLen          uint64     `json:"file_len"`
	MajorBrand       string     `json:"major_brand"`
	CompatibleBrands []string   `json:"compatible_brands"`
	Timescale        uint64     `json:"timescale"`
	Duration         uint64     `json:"duration"`
	Tracks           []mp4Track `json:"tracks"`
}

// mp4ErrorVector mirrors one error vector of reference.json.
type mp4ErrorVector struct {
	Name       string `json:"name"`
	FileSha256 string `json:"file_sha256"`
	FileLen    uint64 `json:"file_len"`
	Error      string `json:"error"`
	Runs       []struct {
		PrefixLen uint64 `json:"prefix_len"`
		Error     string `json:"error"`
	} `json:"runs"`
}

// reference parses the committed reference.json.
func reference(t *testing.T) ([]mp4Vector, []mp4ErrorVector) {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(repoRoot(t), "reference.json"))
	if err != nil {
		t.Fatal(err)
	}
	var parsed struct {
		Vectors []mp4Vector      `json:"vectors"`
		Errors  []mp4ErrorVector `json:"errors"`
	}
	if err := json.Unmarshal(raw, &parsed); err != nil {
		t.Fatal(err)
	}
	return parsed.Vectors, parsed.Errors
}

// wantKind maps a recorded error variant name to its stable kind code.
func wantKind(name string) int32 {
	switch name {
	case "BadValue":
		return ErrBadValue
	case "InvalidMagic":
		return ErrInvalidMagic
	case "TooLarge":
		return ErrTooLarge
	case "Truncated":
		return ErrTruncated
	case "Unsupported":
		return ErrUnsupported
	}
	return 0
}

// TestReferenceVectorsFieldExact replays every committed reference.json
// vector against its committed fixture file and compares field-exact:
// the input's SHA-256 and length, every demuxed fact (brands,
// timescales, per-sample records, stsd entries with avcC/esds hex) —
// the same vectors the Rust gen-reference verify gate and the
// Python/Node SDKs check.
func TestReferenceVectorsFieldExact(t *testing.T) {
	vectors, _ := reference(t)
	for _, want := range vectors {
		t.Run(want.Name, func(t *testing.T) {
			data := fixtureBytes(t, want.Name)
			if uint64(len(data)) != want.FileLen {
				t.Fatalf("%s: fixture is %d bytes, want %d", want.Name, len(data), want.FileLen)
			}
			digest := sha256.Sum256(data)
			if got := hex.EncodeToString(digest[:]); got != want.FileSha256 {
				t.Fatalf("%s: fixture digest %s, want %s", want.Name, got, want.FileSha256)
			}

			raw, err := DemuxCanonical(data)
			if err != nil {
				t.Fatalf("DemuxCanonical(%s): %v", want.Name, err)
			}
			got, err := ParseCanonical(raw)
			if err != nil {
				t.Fatal(err)
			}
			if got.MajorBrand != want.MajorBrand {
				t.Errorf("%s: major brand %q, want %q", want.Name, got.MajorBrand, want.MajorBrand)
			}
			if len(got.CompatibleBrands) != len(want.CompatibleBrands) {
				t.Fatalf("%s: %d compatible brands, want %d", want.Name,
					len(got.CompatibleBrands), len(want.CompatibleBrands))
			}
			for i, brand := range got.CompatibleBrands {
				if brand != want.CompatibleBrands[i] {
					t.Errorf("%s: compatible brand %d = %q, want %q", want.Name, i, brand, want.CompatibleBrands[i])
				}
			}
			if uint64(got.Timescale) != want.Timescale || got.Duration != want.Duration {
				t.Errorf("%s: timescale/duration %d/%d, want %d/%d", want.Name,
					got.Timescale, got.Duration, want.Timescale, want.Duration)
			}
			if len(got.Tracks) != len(want.Tracks) {
				t.Fatalf("%s: %d tracks, want %d", want.Name, len(got.Tracks), len(want.Tracks))
			}
			for ti, track := range got.Tracks {
				wantTrack := want.Tracks[ti]
				if uint64(track.ID) != wantTrack.ID ||
					uint64(track.Timescale) != wantTrack.Timescale ||
					track.Duration != wantTrack.Duration ||
					track.Language != deref(wantTrack.Language) ||
					track.Handler != wantTrack.Handler ||
					track.Width != wantTrack.Width ||
					track.Height != wantTrack.Height {
					t.Errorf("%s track %d: %+v, want id=%d ts=%d dur=%d lang=%s handler=%s %dx%d",
						want.Name, ti, track, wantTrack.ID, wantTrack.Timescale, wantTrack.Duration,
						deref(wantTrack.Language), wantTrack.Handler, wantTrack.Width, wantTrack.Height)
				}
				if uint64(len(track.Samples)) != wantTrack.SampleCount {
					t.Fatalf("%s track %d: %d samples, want %d", want.Name, ti,
						len(track.Samples), wantTrack.SampleCount)
				}
				for si, sample := range track.Samples {
					w := wantTrack.Samples[si]
					if sample.Offset != w.Offset || sample.Size != w.Size ||
						sample.Decoding != w.Decoding ||
						sample.Presentation != int64(w.Presentation) ||
						sample.Duration != w.Duration || sample.Keyframe != w.Keyframe {
						t.Errorf("%s track %d sample %d: %+v, want %+v", want.Name, ti, si, sample, w)
					}
				}
				if uint64(len(track.Entries)) != uint64(len(wantTrack.StsdEntries)) {
					t.Fatalf("%s track %d: %d stsd entries, want %d", want.Name, ti,
						len(track.Entries), len(wantTrack.StsdEntries))
				}
				for ei, entry := range track.Entries {
					w := wantTrack.StsdEntries[ei]
					if entry.Kind != w.Kind || entry.Coding != w.Coding {
						t.Errorf("%s track %d entry %d: %s/%s, want %s/%s", want.Name, ti, ei,
							entry.Kind, entry.Coding, w.Kind, w.Coding)
					}
					switch w.Kind {
					case "Visual":
						if entry.Width != deref32(w.Width) || entry.Height != deref32(w.Height) ||
							entry.AvccHex != deref(w.AvccHex) || entry.EsdsHex != deref(w.EsdsHex) {
							t.Errorf("%s track %d entry %d: visual %+v, want w=%d h=%d avcC=%s esds=%s",
								want.Name, ti, ei, entry, deref32(w.Width), deref32(w.Height),
								deref(w.AvccHex), deref(w.EsdsHex))
						}
					case "Audio":
						if entry.Channels != deref32(w.Channels) || entry.Rate != deref32(w.Rate) ||
							entry.EsdsHex != deref(w.EsdsHex) {
							t.Errorf("%s track %d entry %d: audio %+v, want ch=%d rate=%d esds=%s",
								want.Name, ti, ei, entry, deref32(w.Channels), deref32(w.Rate), deref(w.EsdsHex))
						}
					}
				}
				if wantTrack.AvccHex != nil && track.Entries[0].AvccHex != *wantTrack.AvccHex {
					t.Errorf("%s track %d: track avcC %s, want %s", want.Name, ti,
						track.Entries[0].AvccHex, *wantTrack.AvccHex)
				}
			}
		})
	}
}

func deref(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}

func deref32(v *uint32) uint32 {
	if v == nil {
		return 0
	}
	return *v
}

// TestErrorVectorsRefusedWithRecordedKind checks that every committed
// error fixture is refused with its recorded kind — a status code,
// never a crash.
func TestErrorVectorsRefusedWithRecordedKind(t *testing.T) {
	_, errors := reference(t)
	for _, want := range errors {
		if want.Runs != nil {
			continue // the truncated-prefix campaign has its own test
		}
		t.Run(want.Name, func(t *testing.T) {
			data := fixtureBytes(t, want.Name)
			if uint64(len(data)) != want.FileLen {
				t.Fatalf("%s: fixture is %d bytes, want %d", want.Name, len(data), want.FileLen)
			}
			digest := sha256.Sum256(data)
			if got := hex.EncodeToString(digest[:]); got != want.FileSha256 {
				t.Fatalf("%s: fixture digest %s, want %s", want.Name, got, want.FileSha256)
			}
			_, err := DemuxCanonical(data)
			ffi, ok := err.(*FfiError)
			if !ok {
				t.Fatalf("want FfiError, got %v", err)
			}
			if ffi.Status != StatusRejected {
				t.Errorf("%s: want StatusRejected, got %d", want.Name, ffi.Status)
			}
			if ffi.Kind != wantKind(want.Error) {
				t.Errorf("%s: kind %d, want %d", want.Name, ffi.Kind, wantKind(want.Error))
			}
		})
	}
}

// TestTruncatedPrefixesMatchTheRecordedCampaign replays every recorded
// prefix length of the minimal fixture and checks the recorded kind.
func TestTruncatedPrefixesMatchTheRecordedCampaign(t *testing.T) {
	_, errors := reference(t)
	var campaign *mp4ErrorVector
	for i := range errors {
		if errors[i].Runs != nil {
			campaign = &errors[i]
		}
	}
	if campaign == nil {
		t.Fatal("no truncated-prefix campaign recorded")
	}
	data := fixtureBytes(t, "minimal-4-samples")
	for _, run := range campaign.Runs {
		_, err := DemuxCanonical(data[:run.PrefixLen])
		ffi, ok := err.(*FfiError)
		if !ok {
			t.Fatalf("prefix %d: want FfiError, got %v", run.PrefixLen, err)
		}
		if ffi.Status != StatusRejected || ffi.Kind != wantKind(run.Error) {
			t.Errorf("prefix %d: status %d kind %d, want %d %d", run.PrefixLen,
				ffi.Status, ffi.Kind, StatusRejected, wantKind(run.Error))
		}
	}
}

// TestMalformedInputIsRefused checks the demuxer's refusal path: a
// status code, never a crash.
func TestMalformedInputIsRefused(t *testing.T) {
	if _, err := DemuxCanonical([]byte("not an mp4 file at all, really")); err == nil {
		t.Fatal("garbage input must be refused")
	} else if ffi, ok := err.(*FfiError); !ok || ffi.Status != StatusRejected {
		t.Fatalf("want StatusRejected FfiError, got %v", err)
	}
	if _, err := DemuxCanonical(nil); err == nil {
		t.Fatal("nil input must be refused")
	}
}

// TestFixturePinnedWalkDigest pins the record-walk digest the Rust
// unit tests re-derive, so the binding fails loudly even if the walk
// format drifted between the crate and the SDK bindings.
func TestFixturePinnedWalkDigest(t *testing.T) {
	raw, err := DemuxCanonical(fixtureBytes(t, "minimal-4-samples"))
	if err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(raw)
	const want = "be4b61fc669c451eb943875a47f2971daf92defbe20e58d9551ab29f76f89d24"
	if got := hex.EncodeToString(digest[:]); got != want {
		t.Errorf("minimal-4-samples walk digest %s, want %s", got, want)
	}
	// Walk prologue: major brand isom, three compatible brands.
	wantPrologue := []byte{'i', 's', 'o', 'm', 0, 0, 0, 3}
	for i, b := range wantPrologue {
		if raw[i] != b {
			t.Fatalf("walk byte %d = %d, want %d", i, raw[i], b)
		}
	}
}
