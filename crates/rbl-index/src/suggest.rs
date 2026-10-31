//! Suggestions for what to play after a track: the tracks of the library that fit it, best first.
//!
//! Fork only (not proposed for the core project). The rules are the double-drop searcher's Stage 1 gate, so the two programs
//! rank alike: Camelot key transition, BPM within a tolerance, energy within a tolerance (read from the comment, where Mixed In
//! Key leaves `4A - Energy 7`). Two modes:
//!
//! - [`Mode::Transition`]: BPM within 6 %, energy within 2, every harmonic transition allowed.
//! - [`Mode::Double`]: BPM within 3 %, energy within 1, the key at most one step away on the wheel (same, one number up or down,
//!   or the relative major / minor): the tempo and key have to agree for two tracks to play at once.
//!
//! Each pass is a handful of integer compares a row, like the other views, so it costs a few milliseconds over a whole library.

use crate::key::camelot_rank;
use crate::{Library, Row};

/// How the next track is meant to be played with the last one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Transition,
    Double,
}

impl Mode {
    /// BPM tolerance, as a fraction of the larger tempo.
    fn bpm_tolerance(self) -> f64 {
        match self {
            Self::Transition => 0.06,
            Self::Double => 0.03,
        }
    }

    fn energy_tolerance(self) -> u32 {
        match self {
            Self::Transition => 2,
            Self::Double => 1,
        }
    }

    /// The widest key distance a candidate may sit at (`None`: any harmonic transition).
    fn max_key_distance(self) -> Option<u32> {
        match self {
            Self::Transition => None,
            Self::Double => Some(1),
        }
    }
}

/// A Camelot transition, as the searcher names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Same,
    Step,
    Parallel,
    EnergyBoost,
    Jaws,
    MoodShifter,
}

impl Transition {
    /// What the type adds to the score.
    fn bonus(self) -> f64 {
        match self {
            Self::Same => 0.0,
            Self::Step => 3.0,
            Self::Parallel => 2.0,
            Self::EnergyBoost => 5.0,
            Self::Jaws | Self::MoodShifter => 4.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Same => "SAME",
            Self::Step => "STEP",
            Self::Parallel => "PARALLEL",
            Self::EnergyBoost => "ENERGY_BOOST",
            Self::Jaws => "JAWS",
            Self::MoodShifter => "MOOD_SHIFTER",
        }
    }
}

/// A Camelot key as its wheel number (1 to 12) and mode (`false` minor / `A`, `true` major / `B`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Camelot {
    pub number: u32,
    pub major: bool,
}

impl Camelot {
    /// The key named `name` (a Camelot code or a musical key), or `None` when it has none.
    pub fn of(name: &str) -> Option<Self> {
        let rank = camelot_rank(name);
        (rank < 24).then(|| Self { number: rank / 2 + 1, major: rank % 2 == 1 })
    }

    /// The wheel distance: steps round the ring, plus one for a change of mode.
    pub fn distance(self, other: Self) -> u32 {
        let d = self.number.abs_diff(other.number);
        d.min(12 - d) + u32::from(self.major != other.major)
    }

    /// The harmonic transition from `self` to `other`, or `None` when they do not mix.
    pub fn transition(self, other: Self) -> Option<Transition> {
        let mut ring = (i64::from(other.number) - i64::from(self.number)).rem_euclid(12);
        if ring > 6 {
            ring -= 12;
        }
        let same_mode = self.major == other.major;
        match (same_mode, ring.abs()) {
            (true, 0) => Some(Transition::Same),
            (true, 1) => Some(Transition::Step),
            (false, 0) => Some(Transition::Parallel),
            (true, 2) => Some(Transition::EnergyBoost),
            (true, 5) => Some(Transition::Jaws),
            (false, 3) => Some(Transition::MoodShifter),
            _ => None,
        }
    }
}

/// The energy level in a comment: the number after `Energy` (`4A - Energy 7`), or `None`.
pub fn energy_of(comment: &str) -> Option<u32> {
    let at = comment.to_ascii_lowercase().find("energy")?;
    let rest = comment[at + "energy".len()..].trim_start_matches(|c: char| !c.is_ascii_digit());
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|n| (1..=10).contains(n))
}

/// One suggested track.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub row: Row,
    /// 0 to 100: higher fits better.
    pub score: f64,
    pub key_distance: u32,
    pub transition: Transition,
    /// How far its tempo is from the seed's, in percent of the larger tempo.
    pub bpm_diff_pct: f64,
    pub energy_diff: u32,
    /// How many times it came right after the seed in the library's own sets (plain playlists and history sessions).
    pub followed: u32,
}

/// The preliminary score the searcher gives a candidate: 100, less 8 per key step, 200 per unit of tempo difference and 5
/// per energy level, plus the transition's bonus. Left unclipped: the ranking uses it as it is, so a track the DJ has
/// played after the seed still outranks one that merely fits, when both would show 100.
fn raw_score(key_distance: u32, bpm_diff: f64, energy_diff: u32, transition: Transition) -> f64 {
    100.0 - 8.0 * f64::from(key_distance) - 200.0 * bpm_diff - 5.0 * f64::from(energy_diff) + transition.bonus()
}

/// What having been played after the seed adds: the DJ's own taste outweighs a key step, and a pair played again and again
/// outweighs a pair played once.
fn followed_bonus(times: u32) -> f64 {
    if times == 0 { 0.0 } else { (10.0 + 4.0 * f64::from(times)).min(30.0) }
}

/// How many times each track came right after `seed` in the library's own sets: its plain playlists and its history sessions.
fn followers(library: &Library, seed: Row) -> std::collections::HashMap<Row, u32> {
    fn tally(count: &mut std::collections::HashMap<Row, u32>, members: &[Row], seed: Row) {
        for pair in members.windows(2) {
            if pair[0] == seed && pair[1] != seed {
                *count.entry(pair[1]).or_insert(0) += 1;
            }
        }
    }
    let mut count = std::collections::HashMap::new();
    {
        let playlists = library.playlists();
        for (i, members) in playlists.members.iter().enumerate() {
            // 0 is a plain playlist; a folder holds no tracks and an intelligent one has no order of the DJ's own.
            if playlists.attribute.get(i).copied().unwrap_or(0) == 0 {
                tally(&mut count, members, seed);
            }
        }
    }
    for members in &library.histories().members {
        tally(&mut count, members, seed);
    }
    count
}

/// The suggestions after `seed`, best first, at most `limit`; the rows in `exclude` (and the seed) are left out.
///
/// A seed with no key or no tempo has nothing to be matched on: the answer is empty. A candidate must have both too.
pub fn suggest(library: &Library, seed: Row, mode: Mode, exclude: &dyn Fn(Row) -> bool, limit: usize) -> Vec<Suggestion> {
    let seed_ix = seed as usize;
    let Some(seed_key) = Camelot::of(library.key_name(seed)) else { return Vec::new() };
    let seed_bpm = library.bpm_x100.get(seed_ix).copied().unwrap_or(0);
    if seed_bpm == 0 {
        return Vec::new();
    }
    let seed_energy = energy_of(library.comment.get(seed_ix));
    let followed = followers(library, seed);
    // The unclipped score the ranking sorts on, beside the suggestion it belongs to.
    let mut out: Vec<(f64, Suggestion)> = Vec::new();
    for row in 0..library.len() {
        let Ok(row) = Row::try_from(row) else { break };
        if row == seed || exclude(row) || library.folder_path.get(row as usize).is_empty() {
            continue;
        }
        let bpm = library.bpm_x100.get(row as usize).copied().unwrap_or(0);
        if bpm == 0 {
            continue;
        }
        let bpm_diff = f64::from(bpm.abs_diff(seed_bpm)) / f64::from(bpm.max(seed_bpm));
        if bpm_diff > mode.bpm_tolerance() {
            continue;
        }
        let energy_diff = match (seed_energy, energy_of(library.comment.get(row as usize))) {
            (Some(a), Some(b)) => a.abs_diff(b),
            _ => 0,
        };
        if energy_diff > mode.energy_tolerance() {
            continue;
        }
        let Some(key) = Camelot::of(library.key_name(row)) else { continue };
        let Some(transition) = seed_key.transition(key) else { continue };
        let key_distance = seed_key.distance(key);
        if mode.max_key_distance().is_some_and(|max| key_distance > max) {
            continue;
        }
        let times = followed.get(&row).copied().unwrap_or(0);
        let raw = raw_score(key_distance, bpm_diff, energy_diff, transition) + followed_bonus(times);
        out.push((
            raw,
            Suggestion {
                row,
                score: raw.clamp(0.0, 100.0),
                key_distance,
                transition,
                bpm_diff_pct: bpm_diff * 100.0,
                energy_diff,
                followed: times,
            },
        ));
    }
    // Best first; on a tie the nearer tempo, then the title (a stable, readable order).
    out.sort_by(|(raw_a, a), (raw_b, b)| {
        raw_b
            .total_cmp(raw_a)
            .then(a.bpm_diff_pct.total_cmp(&b.bpm_diff_pct))
            .then_with(|| library.title.get(a.row as usize).cmp(library.title.get(b.row as usize)))
    });
    out.truncate(limit);
    out.into_iter().map(|(_, suggestion)| suggestion).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::testing::{library_from, TestTrack};

    fn track(id: u64, title: &'static str, key: &'static str, bpm: u32, comment: &'static str) -> TestTrack {
        TestTrack { id, title, key, bpm_x100: bpm, comment, path: "D:/m/x.flac", ..TestTrack::default() }
    }

    fn lib() -> Library {
        library_from(&[
            track(1, "Seed", "8A", 17400, "8A - Energy 6"),
            track(2, "Same", "8A", 17400, "8A - Energy 6"),
            track(3, "Neighbour", "9A", 17400, "9A - Energy 6"),
            track(4, "Clash", "3B", 17400, ""),
            track(5, "TooFast", "8A", 19000, ""),
            track(6, "Close", "8A", 17800, ""),
            track(7, "Farther", "8A", 18100, ""),
            track(8, "TwoUp", "10A", 17400, ""),
            track(9, "Loud", "8A", 17400, "8A - Energy 9"),
            track(10, "NoKey", "", 17400, ""),
            track(11, "NoBpm", "8A", 0, ""),
        ])
    }

    fn rows(found: &[Suggestion], lib: &Library) -> Vec<&'static str> {
        found.iter().map(|s| match lib.ids[s.row as usize] {
            2 => "Same", 3 => "Neighbour", 4 => "Clash", 5 => "TooFast", 6 => "Close", 7 => "Farther", 8 => "TwoUp", 9 => "Loud", _ => "?",
        }).collect()
    }

    #[test]
    fn camelot_keys_are_read_from_codes_and_musical_names() {
        assert_eq!(Camelot::of("8A"), Some(Camelot { number: 8, major: false }));
        assert_eq!(Camelot::of("12B"), Some(Camelot { number: 12, major: true }));
        assert_eq!(Camelot::of("Am"), Camelot::of("8A"));
        assert_eq!(Camelot::of(""), None);
    }

    #[test]
    fn transitions_follow_the_searchers_rules() {
        let k = |s| Camelot::of(s).unwrap();
        assert_eq!(k("8A").transition(k("8A")), Some(Transition::Same));
        assert_eq!(k("8A").transition(k("9A")), Some(Transition::Step));
        assert_eq!(k("12A").transition(k("1A")), Some(Transition::Step), "the wheel wraps");
        assert_eq!(k("8A").transition(k("8B")), Some(Transition::Parallel));
        assert_eq!(k("8A").transition(k("10A")), Some(Transition::EnergyBoost));
        assert_eq!(k("8A").transition(k("3A")), Some(Transition::Jaws));
        assert_eq!(k("8A").transition(k("11B")), Some(Transition::MoodShifter));
        assert_eq!(k("8A").transition(k("3B")), None);
        assert_eq!((k("8A").distance(k("9B")), k("12A").distance(k("1A")), k("8A").distance(k("8A"))), (2, 1, 0));
    }

    #[test]
    fn energy_is_read_from_a_mixed_in_key_comment() {
        assert_eq!(energy_of("4A - Energy 7"), Some(7));
        assert_eq!(energy_of("energy:8 something"), Some(8));
        assert_eq!(energy_of("no level here"), None);
        assert_eq!(energy_of("Energy 42"), None, "not a 1 to 10 level");
    }

    #[test]
    fn a_transition_keeps_what_fits_and_ranks_the_closest_first() {
        let lib = lib();
        let found = suggest(&lib, 0, Mode::Transition, &|_| false, 40);
        assert_eq!(rows(&found, &lib)[0], "Same");
        let names = rows(&found, &lib);
        assert!(names.contains(&"Neighbour") && names.contains(&"Close") && names.contains(&"Farther") && names.contains(&"TwoUp"));
        assert!(!names.contains(&"Clash") && !names.contains(&"TooFast"), "a clashing key and a far tempo are out");
        assert!(!names.contains(&"Loud"), "three energy levels away is too many");
        assert_eq!(found.len(), 5, "no key / no tempo / the seed itself are never suggested");
    }

    #[test]
    fn a_double_is_stricter_about_tempo_energy_and_key() {
        let lib = lib();
        let names = rows(&suggest(&lib, 0, Mode::Double, &|_| false, 40), &lib);
        // Close (2.25 % off the tempo: 95.5) outranks Neighbour (one key step, with the step bonus: 95).
        assert_eq!(names, vec!["Same", "Close", "Neighbour"], "Farther is 4% off and TwoUp two steps away");
    }

    #[test]
    fn excluded_rows_and_the_limit_are_honoured() {
        let lib = lib();
        let hidden = lib.row_of_id(2).unwrap();
        let found = suggest(&lib, 0, Mode::Transition, &|r| r == hidden, 2);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|s| s.row != hidden));
    }

    #[test]
    fn a_track_played_after_the_seed_outranks_one_that_merely_fits() {
        use crate::testing::{add_history, add_playlist};
        let mut lib = lib();
        // Seed is row 0. A playlist has Neighbour (row 2) right after it once; two sessions have Close (row 5) after it, and
        // Same (row 1) came *before* it, which is not a follow.
        add_playlist(&mut lib, "set", &[1, 0, 2]);
        add_history(&mut lib, "one", &[0, 5]);
        add_history(&mut lib, "two", &[3, 0, 5]);
        let found = suggest(&lib, 0, Mode::Transition, &|_| false, 40);
        let names = rows(&found, &lib);
        assert_eq!(&names[..3], ["Close", "Neighbour", "Same"], "played twice, then played once, then only fitting");
        let by = |name: &str| found.iter().zip(&names).find(|(_, n)| **n == name).map(|(s, _)| s.clone()).unwrap();
        assert_eq!((by("Close").followed, by("Neighbour").followed, by("Same").followed), (2, 1, 0));
        // The score shown stays within 0 to 100 although the ranking went past it.
        assert!(found.iter().all(|s| (0.0..=100.0).contains(&s.score)));
        assert_eq!(by("Neighbour").score, 100.0);
    }

    #[test]
    fn a_seed_without_a_key_or_a_tempo_has_no_suggestions() {
        let lib = lib();
        for id in [10, 11] {
            let found = suggest(&lib, lib.row_of_id(id).unwrap(), Mode::Transition, &|_| false, 40);
            assert_eq!(found.len(), 0, "track {id} has no key or tempo to match on");
        }
    }
}
