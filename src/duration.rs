//! Episode duration probing.
//!
//! mpv's `watch_later` entries store only the resume position, so the
//! completion threshold needs the duration read from the file's own container
//! header. Symphonia (pure Rust) is used for header probing only, never for
//! decoding.
//!
//! Dispatch is by content, never by file extension. Symphonia's format
//! auto-detection runs first; if it can't identify the file, a small AVI
//! `avih` header reader is tried.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use symphonia::core::formats::FormatOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// The result of probing a video file's duration. An unreadable duration keeps
/// its reason so callers can surface it.
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

/// Determines `path`'s total duration in seconds, trying Symphonia first and
/// the AVI header reader second. Never inspects the extension.
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

/// Reads the container duration via Symphonia's format auto-detection, with
/// no extension hint. Returns `None` if the format is unrecognized, reports no
/// duration, or can't be read.
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

/// Leading bytes searched for the `avih` chunk, which sits in the `hdrl` LIST
/// near the start of a well-formed AVI.
const AVI_SEARCH_WINDOW: usize = 4096;

/// Byte offset of `dwTotalFrames` in the `avih` chunk data, after four `u32`
/// fields (`dwMicroSecPerFrame`, `dwMaxBytesPerSec`, `dwPaddingGranularity`,
/// `dwFlags`).
const AVIH_TOTAL_FRAMES_OFFSET: usize = 16;

/// Reads an AVI `avih` header: duration = total frames × microseconds per
/// frame. Reads only the leading window of the file.
fn probe_avi_fallback(path: &Path) -> Option<f64> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; AVI_SEARCH_WINDOW];
    let n = file.read(&mut buf).ok()?;
    let buf = &buf[..n];

    if buf.len() < 12 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"AVI " {
        return None;
    }

    let tag_pos = buf.windows(4).position(|w| w == b"avih")?;
    // Skip the "avih" tag and the 4-byte chunk size to reach the chunk data.
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

    /// Builds a minimal AVI: a `RIFF`/`AVI ` header, then a `LIST`/`hdrl`
    /// holding an `avih` chunk with the given values and all other fields zero.
    fn fake_avi(micro_sec_per_frame: u32, total_frames: u32) -> Vec<u8> {
        let mut avih_data = vec![0u8; 56];
        avih_data[0..4].copy_from_slice(&micro_sec_per_frame.to_le_bytes());
        avih_data[16..20].copy_from_slice(&total_frames.to_le_bytes());

        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&0u32.to_le_bytes()); // file size, unused
        buf.extend_from_slice(b"AVI ");
        buf.extend_from_slice(b"LIST");
        buf.extend_from_slice(&0u32.to_le_bytes()); // list size, unused
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
    fn avi_fallback_does_not_false_positive_on_avih_bytes_without_a_riff_avi_header() {
        // Junk containing "avih" but no RIFF/AVI header is rejected at the
        // header check, before the "avih" search.
        let mut buf = b"this is not a riff file but it does contain the substring ".to_vec();
        buf.extend_from_slice(b"avih");
        buf.extend_from_slice(b" somewhere in the middle of otherwise unrelated junk");
        let file = write_temp(&buf);

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
        // A real MKV saved as `.avi` must still probe as Known; extension
        // dispatch would send it to the AVI reader and yield Unknown.
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

    // Corrupt or non-video files resolve to `Unknown` without panic or hang.

    #[test]
    fn duration_unknown_for_a_zero_byte_file_no_panic() {
        let file = write_temp(b"");
        let outcome = probe_duration(file.path());
        assert_eq!(outcome, DurationOutcome::Unknown(DurationError::Unreadable));
    }

    #[test]
    fn duration_unknown_for_a_plain_text_file_renamed_with_a_video_extension() {
        // Text content under a video extension, standing in for a corrupt
        // download.
        let dir = tempfile::tempdir().unwrap();
        let fake_video = dir.path().join("Not Actually A Video.mkv");
        std::fs::write(
            &fake_video,
            b"Episode notes:\n1. Great episode\n2. Cliffhanger\n",
        )
        .unwrap();

        let outcome = probe_duration(&fake_video);
        assert_eq!(outcome, DurationOutcome::Unknown(DurationError::Unreadable));
    }
}
