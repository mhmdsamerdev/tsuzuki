//! Episode identity via Anitomy (CLAUDE.md's "Anitomy only annotates,
//! never reorders", design.md §5).
//!
//! `episode_identity` takes files that are already correctly ordered (by
//! `series::ordered_episodes`/`ordered_episodes_excluding` — natural sort
//! remains the sole authority for *order*) and annotates each with an
//! episode number for MAL sync identity. This module has no caller yet
//! (Phase 7 wires it into sync); it only needs to be callable and
//! independently testable.

use std::path::{Path, PathBuf};

use tsuzuki_anitomy::ElementKind;

/// One ordered episode file, annotated with its resolved episode number.
///
/// Unused outside tests until Phase 7 wires this into MAL sync — this
/// phase only needs the module to be callable and independently testable,
/// per CLAUDE.md, matching `series.rs`'s same not-yet-wired convention.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeIdentity {
    pub path: PathBuf,
    /// 1-based position in the natural-sort-ordered list this was built
    /// from. Never used to reorder anything — only as the fallback source
    /// for `episode_number` when Anitomy can't parse one.
    pub ordinal: usize,
    /// Anitomy's parsed episode number, or `ordinal` when Anitomy found no
    /// `Episode` element or its value didn't parse as a number. This
    /// fallback is expected/common, not an error — silently taken, per
    /// CLAUDE.md's "Anitomy only annotates, never reorders".
    pub episode_number: u32,
}

/// Annotates each already-ordered file with its resolved episode number.
/// `ordered_files` must already be in the order `series::ordered_episodes`
/// (or `ordered_episodes_excluding`) produced — this function never
/// reorders its input.
#[allow(dead_code)]
pub fn episode_identity(ordered_files: &[PathBuf]) -> Vec<EpisodeIdentity> {
    ordered_files
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let ordinal = index + 1;
            let episode_number = parsed_episode_number(path).unwrap_or(ordinal as u32);
            EpisodeIdentity {
                path: path.clone(),
                ordinal,
                episode_number,
            }
        })
        .collect()
}

/// Parses `path`'s filename via Anitomy and returns its episode number, if
/// any. Always calls `.file_name()` before handing anything to
/// `tsuzuki_anitomy::parse` — passing a path with directory components
/// silently breaks episode extraction (verified by direct testing; see
/// CLAUDE.md's "Anitomy must be called with a bare filename, never a
/// full/relative path").
fn parsed_episode_number(path: &Path) -> Option<u32> {
    let filename = path.file_name()?.to_str()?;
    tsuzuki_anitomy::parse(filename)
        .into_iter()
        .find(|element| element.kind() == ElementKind::Episode)
        .and_then(|element| element.value().parse::<u32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parseable_filename_uses_anitomys_episode_number() {
        let files = vec![PathBuf::from("[Group] Show Name - 05 [1080p].mkv")];
        let identities = episode_identity(&files);

        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].ordinal, 1);
        assert_eq!(identities[0].episode_number, 5);
    }

    #[test]
    fn an_unparseable_filename_falls_back_to_ordinal_position() {
        // Anitomy finds no `Episode` element in this filename at all —
        // confirmed by direct testing against the real crate.
        let files = vec![
            PathBuf::from("[Group] Show Name - 01.mkv"),
            PathBuf::from("not-parseable-junk-file.mkv"),
        ];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 1);
        // Second file: no Episode element, so falls back to its ordinal
        // position (2), not some other guess.
        assert_eq!(identities[1].ordinal, 2);
        assert_eq!(identities[1].episode_number, 2);
    }

    #[test]
    fn a_numbering_gap_uses_parsed_numbers_not_ordinal_positions() {
        // Files 1, 2, 3, 5, 6 on disk (episode 4 missing/never released),
        // all parseable. This is the actual point of Anitomy per
        // design.md §5: the *parsed* numbers must be used, including the
        // gap — ordinal position (which would say 1,2,3,4,5) must not be
        // substituted in just because the sequence isn't consecutive.
        let files = vec![
            PathBuf::from("[Group] Show - 01 [1080p].mkv"),
            PathBuf::from("[Group] Show - 02 [1080p].mkv"),
            PathBuf::from("[Group] Show - 03 [1080p].mkv"),
            PathBuf::from("[Group] Show - 05 [1080p].mkv"),
            PathBuf::from("[Group] Show - 06 [1080p].mkv"),
        ];
        let identities = episode_identity(&files);

        let episode_numbers: Vec<u32> = identities.iter().map(|i| i.episode_number).collect();
        assert_eq!(episode_numbers, vec![1, 2, 3, 5, 6]);

        let ordinals: Vec<usize> = identities.iter().map(|i| i.ordinal).collect();
        assert_eq!(ordinals, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_path_with_directory_components_still_parses_correctly() {
        // Proves the `.file_name()` fix actually works, not just that it
        // exists in the code: passing the *full* path (with a "Season 1/"
        // directory component) straight to `tsuzuki_anitomy::parse`
        // without stripping first silently loses the episode number
        // entirely (confirmed by direct testing) — so this test would
        // fail if `parsed_episode_number` ever stopped calling
        // `.file_name()` before parsing.
        let files = vec![PathBuf::from("Season 1/ep02.mkv")];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 2);
    }

    // --- Gap audit: edge cases beyond the happy path ---

    #[test]
    fn an_episode_element_that_parses_but_isnt_a_valid_u32_falls_back_to_ordinal() {
        // Anitomy successfully extracts an `Episode` element here — it's
        // not a parse *failure* the way "no Episode element at all" is —
        // but "05.5" (a half-episode/recap numbering scheme real anime
        // releases use) doesn't fit `u32`. Confirmed by direct testing:
        // Anitomy really does hand back the literal string "05.5", not a
        // pre-validated integer. `episode_number`'s `.parse::<u32>().ok()`
        // must treat this the same as no-Episode-at-all and fall back to
        // ordinal, not panic or silently truncate.
        let files = vec![PathBuf::from("[Group] Show - 05.5 [1080p].mkv")];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 1);
    }

    #[test]
    fn an_empty_file_list_returns_an_empty_vec_without_panicking() {
        let identities = episode_identity(&[]);
        assert!(identities.is_empty());
    }

    #[test]
    fn duplicate_parsed_episode_numbers_across_different_files_are_silently_allowed() {
        // Two different releases of the same nominal episode (e.g. two
        // release groups) both parse to episode 5. `episode_identity`
        // does not detect or flag this — deliberately: deciding which
        // file is "really" episode 5 (preferring one release over
        // another, merging them, erroring) is a judgment call this
        // annotation-only module has no business making, per CLAUDE.md's
        // "never infer" (§2) and "Anitomy only annotates, never
        // reorders". That decision belongs to whatever consumes this
        // data (Phase 7's MAL sync), not here. This test locks in and
        // documents that current, deliberate behavior rather than
        // leaving it implicit.
        let files = vec![
            PathBuf::from("[GroupA] Show - 05 [1080p].mkv"),
            PathBuf::from("[GroupB] Show - 05 [720p].mkv"),
        ];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 5);
        assert_eq!(identities[1].episode_number, 5);
        assert_eq!(identities.len(), 2);
    }

    #[test]
    fn a_purely_numeric_filename_has_no_context_for_anitomy_to_recognize_as_an_episode() {
        // Confirmed by direct testing: a bare "5.mkv" with no other
        // context parses as `Title = "5"`, not `Episode = "5"` — Anitomy
        // needs surrounding context (a release group, brackets, a
        // preceding dash, etc.) to recognize a number as an episode, not
        // just numeric-ness. This is a real-world naming scheme (loose
        // single-digit filenames), so the fallback path matters here,
        // not just for filenames Anitomy can't parse at all.
        let files = vec![PathBuf::from("5.mkv")];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 1);
    }

    #[test]
    fn a_file_with_no_parent_directory_component_still_resolves_its_filename() {
        // Already at the root of a tracked root (not nested in a series
        // subfolder) — `Path::file_name()` on a single-component path
        // must still return that component, not `None`.
        let files = vec![PathBuf::from("[Group] Show - 03 [1080p].mkv")];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 3);
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_filename_falls_back_to_ordinal_without_panicking() {
        // `Path::file_name()` succeeds (it works on raw OS bytes), but
        // `.to_str()` returns `None` for invalid UTF-8 — the `?` chain in
        // `parsed_episode_number` must treat that the same as any other
        // "couldn't determine an episode number" case, not panic.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let invalid_utf8 = OsStr::from_bytes(&[0x66, 0x6f, 0xff, 0x6f, b'.', b'm', b'k', b'v']); // "fo\xFFo.mkv"
        let files = vec![PathBuf::from(invalid_utf8)];
        let identities = episode_identity(&files);

        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].episode_number, 1);
    }

    #[test]
    fn a_multi_episode_range_deterministically_takes_the_first_episode_element() {
        // Confirmed by direct testing: Anitomy parses an episode range
        // like "01-02" as *two separate* `Episode` elements ("01" and
        // "02"), not one range string. `parsed_episode_number` uses
        // `.find()`, which deterministically takes the first — this test
        // exists to make that choice visible and locked-in (a real,
        // observable behavior), not to claim it's the only defensible
        // one. Neither CLAUDE.md nor design.md §5 specifies range
        // handling; if that ever needs to change, this test should fail
        // and be the prompt to make the new behavior equally explicit.
        let files = vec![PathBuf::from("[Group] Show - 01-02 [1080p].mkv")];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 1);
    }
}
