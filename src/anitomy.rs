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
}
