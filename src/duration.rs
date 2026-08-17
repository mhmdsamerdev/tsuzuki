//! Episode duration probing (CLAUDE.md's "Completion threshold needs
//! episode duration").
//!
//! mpv's `watch_later` entries only ever store the resume position, never a
//! file's total duration, so the completion threshold (§8:
//! `completion_remaining_seconds`) needs Tsuzuki to read duration itself,
//! from the video file's own container header. `symphonia` (pure Rust) does
//! this for metadata/probe purposes only — this module never asks it to
//! decode actual audio or video frames, only to read format headers.
//!
//! Dispatch is by content-probe, never by file extension: [`probe_duration`]
//! never looks at `path`'s extension at all, only its bytes. Symphonia's own
//! format auto-detection is tried first; if it can't identify the file, a
//! small hand-written fallback reader for AVI's `avih` RIFF header is tried
//! next — reached on probe failure, not gated on the `.avi` extension.
//!
//! No caller yet (Phase 5 wires this into the completion-threshold check via
//! `mpv::classify`) — `#[allow(dead_code)]`, matching `series.rs`/
//! `anitomy.rs`'s established not-yet-wired convention.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use symphonia::core::formats::FormatOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// The result of probing a video file's duration. Never collapses an
/// unreadable duration to a bare `None` — the reason is preserved so a
/// later phase (Phase 5's completion check, or `doctor`) can decide whether
/// and how to surface it, per CLAUDE.md's "Unreadable duration is
/// surfaced, never silently dropped."
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum DurationOutcome {
    Known(f64),
    Unknown(DurationError),
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurationError {
    /// The file couldn't even be opened (missing, permission denied, ...).
    Io(String),
    /// Neither Symphonia's probe nor the AVI fallback could identify or
    /// parse the file's container format.
    Unreadable,
}

/// Determines `path`'s total duration in seconds. Tries Symphonia's own
/// content-based format probing first; if that fails to identify or read
/// the file, falls back to a hand-written AVI `avih` RIFF header reader.
/// Neither attempt ever inspects `path`'s extension.
#[allow(dead_code)]
pub fn probe_duration(path: &Path) -> DurationOutcome {
    if let Some(secs) = probe_with_symphonia(path) {
        return DurationOutcome::Known(secs);
    }
    if let Some(secs) = probe_avi_fallback(path) {
        return DurationOutcome::Known(secs);
    }
    match File::open(path) {
        Ok(_) => DurationOutcome::Unknown(DurationError::Unreadable),
        Err(e) => DurationOutcome::Unknown(DurationError::Io(e.to_string())),
    }
}

/// Hands `path` to Symphonia's own format auto-detection (no extension
/// hint given at all — content is the only signal used) and reads the
/// container's overall duration from `FormatReader::media_info()`. Returns
/// `None` on any failure (unsupported/unrecognized format, no duration
/// reported, I/O error) — the caller decides what to try next.
fn probe_with_symphonia(path: &Path) -> Option<f64> {
    let file = File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let hint = Hint::new();

    let format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;

    let media_info = format.media_info();
    let time_base = media_info.time_base?;
    let duration = media_info.duration?;
    let time = time_base.calc_duration(duration)?;
    Some(time.as_secs_f64())
}

/// The size of the leading window read from the file when searching for an
/// AVI `avih` chunk. A well-formed AVI's `hdrl` LIST — which `avih` is
/// always the first member of — sits near the very start of the file, so
/// there's no need to read further than this to find it.
const AVI_SEARCH_WINDOW: usize = 4096;

/// `AVIMAINHEADER`'s `dwTotalFrames` field offset within the `avih` chunk's
/// data, in bytes: `dwMicroSecPerFrame`, `dwMaxBytesPerSec`,
/// `dwPaddingGranularity`, `dwFlags` each precede it as one `u32` (4 bytes)
/// apiece.
const AVIH_TOTAL_FRAMES_OFFSET: usize = 16;

/// Hand-written reader for AVI's `avih` RIFF header — duration =
/// total-frames × microseconds-per-frame. Only reached when Symphonia's own
/// probe couldn't identify the file (never gated on a `.avi` extension).
/// Reads only a small leading window of the file, never the whole thing.
fn probe_avi_fallback(path: &Path) -> Option<f64> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; AVI_SEARCH_WINDOW];
    let n = file.read(&mut buf).ok()?;
    let buf = &buf[..n];

    if buf.len() < 12 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"AVI " {
        return None;
    }

    let tag_pos = buf.windows(4).position(|w| w == b"avih")?;
    // Skip the 4-byte "avih" tag itself and its 4-byte chunk-size field to
    // reach the start of the chunk's data.
    let data_start = tag_pos + 8;
    if buf.len() < data_start + AVIH_TOTAL_FRAMES_OFFSET + 4 {
        return None;
    }

    let micro_sec_per_frame = u32::from_le_bytes(buf[data_start..data_start + 4].try_into().ok()?);
    let frames_start = data_start + AVIH_TOTAL_FRAMES_OFFSET;
    let total_frames = u32::from_le_bytes(buf[frames_start..frames_start + 4].try_into().ok()?);

    if micro_sec_per_frame == 0 {
        return None;
    }

    Some(f64::from(total_frames) * f64::from(micro_sec_per_frame) / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Builds a minimal-but-valid AVI byte layout: a `RIFF`/`AVI ` header
    /// followed directly by a `LIST`/`hdrl` containing an `avih` chunk with
    /// the given `dwMicroSecPerFrame`/`dwTotalFrames` values. Every other
    /// `AVIMAINHEADER` field is zeroed — this reader never looks at them.
    fn fake_avi(micro_sec_per_frame: u32, total_frames: u32) -> Vec<u8> {
        let mut avih_data = vec![0u8; 56];
        avih_data[0..4].copy_from_slice(&micro_sec_per_frame.to_le_bytes());
        avih_data[16..20].copy_from_slice(&total_frames.to_le_bytes());

        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&0u32.to_le_bytes()); // file size, unused by this reader
        buf.extend_from_slice(b"AVI ");
        buf.extend_from_slice(b"LIST");
        buf.extend_from_slice(&0u32.to_le_bytes()); // list size, unused by this reader
        buf.extend_from_slice(b"hdrl");
        buf.extend_from_slice(b"avih");
        buf.extend_from_slice(&(avih_data.len() as u32).to_le_bytes());
        buf.extend_from_slice(&avih_data);
        buf
    }

    fn write_temp(bytes: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn avi_fallback_computes_duration_from_frames_times_microseconds() {
        // 30 fps (33_333 microseconds/frame) * 150 frames = 5.0 seconds.
        let bytes = fake_avi(33_333, 150);
        let file = write_temp(&bytes);

        let secs = probe_avi_fallback(file.path()).unwrap();
        assert!((secs - 4.99995).abs() < 0.001, "expected ~5.0s, got {secs}");
    }

    #[test]
    fn avi_fallback_returns_none_for_non_riff_bytes() {
        let file = write_temp(b"not an avi file at all, just junk bytes");
        assert!(probe_avi_fallback(file.path()).is_none());
    }

    #[test]
    fn avi_fallback_returns_none_when_avih_chunk_is_missing() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(b"AVI ");
        buf.extend_from_slice(b"LIST");
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(b"hdrl");
        // No "avih" tag anywhere.
        let file = write_temp(&buf);

        assert!(probe_avi_fallback(file.path()).is_none());
    }

    #[test]
    fn avi_fallback_returns_none_when_micro_sec_per_frame_is_zero() {
        let bytes = fake_avi(0, 150);
        let file = write_temp(&bytes);
        assert!(probe_avi_fallback(file.path()).is_none());
    }

    #[test]
    fn symphonia_reads_duration_from_a_real_mkv_fixture() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/short.mkv"
        ));
        let secs = probe_with_symphonia(path).expect("symphonia should read the mkv fixture");
        assert!(
            (secs - 5.0).abs() < 0.5,
            "expected ~5s duration, got {secs}"
        );
    }

    #[test]
    fn symphonia_reads_duration_from_a_real_mp4_fixture() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/short.mp4"
        ));
        let secs = probe_with_symphonia(path).expect("symphonia should read the mp4 fixture");
        assert!(
            (secs - 5.0).abs() < 0.5,
            "expected ~5s duration, got {secs}"
        );
    }

    #[test]
    fn probe_duration_prefers_symphonia_over_avi_fallback_for_a_real_mkv() {
        let path = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/short.mkv"
        ));
        let outcome = probe_duration(path);
        assert!(matches!(outcome, DurationOutcome::Known(secs) if (secs - 5.0).abs() < 0.5));
    }

    #[test]
    fn probe_duration_via_avi_fallback_when_symphonia_cannot_identify_the_file() {
        let bytes = fake_avi(33_333, 150);
        let file = write_temp(&bytes);

        let outcome = probe_duration(file.path());
        assert!(matches!(outcome, DurationOutcome::Known(secs) if (secs - 5.0).abs() < 0.001));
    }

    #[test]
    fn probe_duration_is_reached_via_content_probe_not_the_avi_extension() {
        // A real, Symphonia-parseable MKV file, saved with a `.avi`
        // extension. Symphonia must still succeed on content alone — this
        // is what proves `probe_duration` never dispatches by extension:
        // if it did (a hardcoded ".avi" -> AVI-reader branch), this file
        // would be handed to `probe_avi_fallback`, which would fail to
        // find a RIFF/AVI header in real MKV bytes and this test would see
        // `Unknown`, not `Known`.
        let real_mkv_bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/short.mkv"
        ))
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let renamed = dir.path().join("episode.avi");
        std::fs::write(&renamed, &real_mkv_bytes).unwrap();

        let outcome = probe_duration(&renamed);
        assert!(
            matches!(outcome, DurationOutcome::Known(secs) if (secs - 5.0).abs() < 0.5),
            "expected Symphonia to succeed via content-probe despite the .avi extension, got: {outcome:?}"
        );
    }

    #[test]
    fn duration_unknown_when_neither_symphonia_nor_the_avi_fallback_can_read_the_file() {
        let file = write_temp(b"this is neither a real container nor a RIFF/AVI file");

        let outcome = probe_duration(file.path());
        assert_eq!(outcome, DurationOutcome::Unknown(DurationError::Unreadable));
    }

    #[test]
    fn duration_unknown_carries_an_io_reason_when_the_file_does_not_exist() {
        let outcome = probe_duration(Path::new("/definitely/does/not/exist/tsuzuki-test.mkv"));
        assert!(matches!(
            outcome,
            DurationOutcome::Unknown(DurationError::Io(_))
        ));
    }
}
