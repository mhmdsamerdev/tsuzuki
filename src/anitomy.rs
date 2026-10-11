//! Episode identity for MAL sync. Anitomy labels files that natural sort has
//! already ordered; it never reorders them.

use std::path::{Path, PathBuf};

use tsuzuki_anitomy::ElementKind;

/// One ordered episode file with its resolved episode number.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeIdentity {
    pub path: PathBuf,
    /// 1-based position in the natural-sort order. Only the fallback for
    /// `episode_number`.
    pub ordinal: usize,
    /// Anitomy's episode number, or `ordinal` when Anitomy finds none or it
    /// isn't a `u32`. The fallback is routine, not an error.
    pub episode_number: u32,
    /// Anitomy's season, only when it also found the episode number.
    pub season: Option<u32>,
}

/// Short form for listings: `ep. 1`, or `S02E01` past season 1.
pub fn episode_label(season: Option<u32>, episode: u32) -> String {
    match season {
        Some(season) if season != 1 => format!("S{season:02}E{episode:02}"),
        _ => format!("ep. {episode}"),
    }
}

/// Long form for resume/start: `episode 1`, or `Season 2, Episode 1` past season 1.
pub fn episode_phrase(season: Option<u32>, episode: u32) -> String {
    match season {
        Some(season) if season != 1 => format!("Season {season}, Episode {episode}"),
        _ => format!("episode {episode}"),
    }
}

/// Labels each file with its episode number. `ordered_files` must already be
/// in natural-sort order; the order is kept as given.
#[allow(dead_code)]
pub fn episode_identity(ordered_files: &[PathBuf]) -> Vec<EpisodeIdentity> {
    ordered_files
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let ordinal = index + 1;
            let (episode_number, season) = match parsed_episode(path) {
                Some((episode, season)) => (episode, season),
                None => (ordinal as u32, None),
            };
            EpisodeIdentity {
                path: path.clone(),
                ordinal,
                episode_number,
                season,
            }
        })
        .collect()
}

/// Anitomy's episode number and season for `path`'s filename, if it found an
/// episode. Anitomy gets the bare filename only; a path with directories loses
/// the episode.
fn parsed_episode(path: &Path) -> Option<(u32, Option<u32>)> {
    let filename = path.file_name()?.to_str()?;
    let elements = tsuzuki_anitomy::parse(filename);
    let first = |kind| {
        elements
            .iter()
            .find(|element| element.kind() == kind)
            .and_then(|element| element.value().parse::<u32>().ok())
    };
    let episode = first(ElementKind::Episode)?;
    Some((episode, first(ElementKind::Season)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_label_shows_season_only_past_season_one() {
        assert_eq!(episode_label(None, 1), "ep. 1");
        assert_eq!(episode_label(Some(1), 1), "ep. 1");
        assert_eq!(episode_label(Some(2), 1), "S02E01");
        assert_eq!(episode_label(Some(10), 12), "S10E12");
        assert_eq!(episode_label(Some(0), 3), "S00E03");
    }

    #[test]
    fn episode_phrase_shows_season_only_past_season_one() {
        assert_eq!(episode_phrase(None, 1), "episode 1");
        assert_eq!(episode_phrase(Some(1), 4), "episode 4");
        assert_eq!(episode_phrase(Some(2), 1), "Season 2, Episode 1");
        assert_eq!(episode_phrase(Some(10), 12), "Season 10, Episode 12");
    }

    #[test]
    fn season_is_read_from_the_filename_when_anitomy_finds_it() {
        let files = vec![
            PathBuf::from("[Judas] Bakuman - S02E01.mkv"),
            PathBuf::from("[Judas] Bakuman - S01E03.mkv"),
            PathBuf::from("[Group] Show Name - 05 [1080p].mkv"),
        ];
        let seasons: Vec<(u32, Option<u32>)> = episode_identity(&files)
            .iter()
            .map(|i| (i.episode_number, i.season))
            .collect();
        assert_eq!(seasons, vec![(1, Some(2)), (3, Some(1)), (5, None)]);
    }

    #[test]
    fn season_is_none_when_the_episode_falls_back_to_ordinal() {
        let files = vec![PathBuf::from("not-parseable-junk-file.mkv")];
        let identities = episode_identity(&files);
        assert_eq!(identities[0].episode_number, 1);
        assert_eq!(identities[0].season, None);
    }

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
        // Anitomy finds no `Episode` element in the second name.
        let files = vec![
            PathBuf::from("[Group] Show Name - 01.mkv"),
            PathBuf::from("not-parseable-junk-file.mkv"),
        ];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 1);
        assert_eq!(identities[1].ordinal, 2);
        assert_eq!(identities[1].episode_number, 2);
    }

    #[test]
    fn a_numbering_gap_uses_parsed_numbers_not_ordinal_positions() {
        // Episode 4 is missing; parsed numbers keep the gap instead of
        // collapsing to ordinals 1..=5.
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
        // Fails if the parent directory reaches Anitomy, which then loses
        // the episode.
        let files = vec![PathBuf::from("Season 1/ep02.mkv")];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 2);
    }

    #[test]
    fn an_episode_element_that_parses_but_isnt_a_valid_u32_falls_back_to_ordinal() {
        // Anitomy returns the literal "05.5" (a half episode), which isn't a
        // `u32`; it must fall back, not truncate.
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
        // Two releases of episode 5 both keep number 5. Picking one would be
        // a guess, so it is left to the consumer.
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
        // Anitomy reads a bare "5.mkv" as the title, not an episode.
        let files = vec![PathBuf::from("5.mkv")];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 1);
    }

    #[test]
    fn a_file_with_no_parent_directory_component_still_resolves_its_filename() {
        // A single-component path still yields its filename.
        let files = vec![PathBuf::from("[Group] Show - 03 [1080p].mkv")];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 3);
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_filename_falls_back_to_ordinal_without_panicking() {
        // `.to_str()` returns `None` for invalid UTF-8; that must fall back,
        // not panic.
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
        // Anitomy splits "01-02" into two `Episode` elements; the first wins.
        let files = vec![PathBuf::from("[Group] Show - 01-02 [1080p].mkv")];
        let identities = episode_identity(&files);

        assert_eq!(identities[0].episode_number, 1);
    }
}
