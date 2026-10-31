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

use std::collections::{HashMap, HashSet};

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
    /// What the ranking sorts on: the score before it is clipped to 100, with the follow bonus in.
    pub rank: f64,
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

/// How many times each track came right after another in the library's own sets: its plain playlists and its history sessions.
struct Followed(HashMap<(Row, Row), u32>);

impl Followed {
    fn of(library: &Library) -> Self {
        fn tally(count: &mut HashMap<(Row, Row), u32>, members: &[Row]) {
            for pair in members.windows(2) {
                if pair[0] != pair[1] {
                    *count.entry((pair[0], pair[1])).or_insert(0) += 1;
                }
            }
        }
        let mut count = HashMap::new();
        {
            let playlists = library.playlists();
            for (i, members) in playlists.members.iter().enumerate() {
                // 0 is a plain playlist; a folder holds no tracks and an intelligent one has no order of the DJ's own.
                if playlists.attribute.get(i).copied().unwrap_or(0) == 0 {
                    tally(&mut count, members);
                }
            }
        }
        for members in &library.histories().members {
            tally(&mut count, members);
        }
        Self(count)
    }

    fn times(&self, seed: Row, row: Row) -> u32 {
        self.0.get(&(seed, row)).copied().unwrap_or(0)
    }
}

/// The library read once for a run of passes: each row's key and energy parsed, the sets' neighbouring pairs counted.
struct Prepared<'a> {
    library: &'a Library,
    key: Vec<Option<Camelot>>,
    energy: Vec<Option<u32>>,
    followed: Followed,
}

/// A stable shuffle of a row for one `variety`: breaks ties between equally good tracks differently each time.
fn mix(row: Row, variety: u64) -> u64 {
    let mut x = u64::from(row) ^ variety.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl<'a> Prepared<'a> {
    fn new(library: &'a Library) -> Self {
        let rows = (0..library.len()).filter_map(|i| Row::try_from(i).ok());
        Self {
            library,
            key: rows.clone().map(|r| Camelot::of(library.key_name(r))).collect(),
            energy: rows.map(|r| energy_of(library.comment.get(r as usize))).collect(),
            followed: Followed::of(library),
        }
    }

    fn bpm(&self, row: Row) -> u32 {
        self.library.bpm_x100.get(row as usize).copied().unwrap_or(0)
    }

    /// The suggestions after `seed`, best first, at most `limit`. `variety` 0 breaks a tie by the nearer tempo and then the
    /// title; any other value by a shuffle of the rows that value picks.
    fn rank(&self, seed: Row, mode: Mode, exclude: &dyn Fn(Row) -> bool, limit: usize, variety: u64) -> Vec<Suggestion> {
        let seed_ix = seed as usize;
        let Some(seed_key) = self.key.get(seed_ix).copied().flatten() else { return Vec::new() };
        let seed_bpm = self.bpm(seed);
        if seed_bpm == 0 {
            return Vec::new();
        }
        let seed_energy = self.energy.get(seed_ix).copied().flatten();
        // The unclipped score the ranking sorts on is kept in `Suggestion::rank`.
        let mut out: Vec<Suggestion> = Vec::new();
        for (ix, key) in self.key.iter().enumerate() {
            let Ok(row) = Row::try_from(ix) else { break };
            let Some(key) = key else { continue };
            if row == seed || exclude(row) || self.library.folder_path.get(ix).is_empty() {
                continue;
            }
            let bpm = self.bpm(row);
            if bpm == 0 {
                continue;
            }
            let bpm_diff = f64::from(bpm.abs_diff(seed_bpm)) / f64::from(bpm.max(seed_bpm));
            if bpm_diff > mode.bpm_tolerance() {
                continue;
            }
            let energy_diff = match (seed_energy, self.energy[ix]) {
                (Some(a), Some(b)) => a.abs_diff(b),
                _ => 0,
            };
            if energy_diff > mode.energy_tolerance() {
                continue;
            }
            let Some(transition) = seed_key.transition(*key) else { continue };
            let key_distance = seed_key.distance(*key);
            if mode.max_key_distance().is_some_and(|max| key_distance > max) {
                continue;
            }
            let times = self.followed.times(seed, row);
            let rank = raw_score(key_distance, bpm_diff, energy_diff, transition) + followed_bonus(times);
            out.push(Suggestion {
                row,
                score: rank.clamp(0.0, 100.0),
                rank,
                key_distance,
                transition,
                bpm_diff_pct: bpm_diff * 100.0,
                energy_diff,
                followed: times,
            });
        }
        out.sort_by(|a, b| {
            let order = b.rank.total_cmp(&a.rank);
            if variety == 0 {
                order
                    .then(a.bpm_diff_pct.total_cmp(&b.bpm_diff_pct))
                    .then_with(|| self.library.title.get(a.row as usize).cmp(self.library.title.get(b.row as usize)))
            } else {
                order.then_with(|| mix(a.row, variety).cmp(&mix(b.row, variety)))
            }
        });
        out.truncate(limit);
        out
    }
}

/// The suggestions after `seed`, best first, at most `limit`; the rows in `exclude` (and the seed) are left out.
///
/// A seed with no key or no tempo has nothing to be matched on: the answer is empty. A candidate must have both too.
pub fn suggest(library: &Library, seed: Row, mode: Mode, exclude: &dyn Fn(Row) -> bool, limit: usize) -> Vec<Suggestion> {
    Prepared::new(library).rank(seed, mode, exclude, limit, 0)
}

/// One step of a built chain: how this track is to be played with the one before it, and why it fits.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub mode: Mode,
    pub suggestion: Suggestion,
}

/// A run of tracks built from a seed: each one fits the one before it as its step's mode asks.
#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    pub seed: Row,
    pub steps: Vec<Step>,
    /// The steps' ranks added up: what the search maximised.
    pub rank: f64,
}

/// The pattern in `text` as modes: `T` (a transition) and `D` (a double drop), either case, one letter a track. Spaces and
/// the usual separators between letters are ignored; anything else, or nothing at all, is no pattern.
pub fn parse_pattern(text: &str) -> Option<Vec<Mode>> {
    let mut modes = Vec::new();
    for c in text.chars() {
        match c.to_ascii_uppercase() {
            'T' => modes.push(Mode::Transition),
            'D' => modes.push(Mode::Double),
            c if c.is_whitespace() || matches!(c, ',' | '-' | '>' | '/' | '+' | '·') => {}
            _ => return None,
        }
    }
    (!modes.is_empty()).then_some(modes)
}

/// Chains kept at each step of the search.
const BEAM: usize = 10;
/// Candidates tried after each chain at each step.
const BRANCH: usize = 6;
/// The furthest any track may stray from the seed's tempo, as a fraction of the larger one: a chain of steps each within
/// tolerance of the last would otherwise wander off.
const MAX_DRIFT: f64 = 0.08;

/// Builds a chain of up to `length` tracks after `seed`, the `i`-th fitting the one before as `pattern[i % pattern.len()]`
/// asks (a pattern shorter than the length repeats). A beam search over [`Prepared::rank`], maximising the ranks added up,
/// so a track the DJ has played after the last one is taken when it leads somewhere that fits too.
///
/// No track is used twice, and none in `exclude`, and none strays from the seed's tempo by more than 8 %. The chain is
/// shorter than `length` when nothing fits after its end. `variety` 0 is the plain ranking; any other value shuffles the
/// order among equally good tracks, so asking again with a new one gives another chain.
pub fn build_chain(library: &Library, seed: Row, pattern: &[Mode], length: usize, exclude: &dyn Fn(Row) -> bool, variety: u64) -> Chain {
    let prepared = Prepared::new(library);
    let seed_bpm = prepared.bpm(seed);
    let strays = |row: Row| {
        let bpm = prepared.bpm(row);
        bpm == 0 || seed_bpm == 0 || f64::from(bpm.abs_diff(seed_bpm)) / f64::from(bpm.max(seed_bpm)) > MAX_DRIFT
    };
    let mut beam: Vec<(f64, Vec<Step>)> = vec![(0.0, Vec::new())];
    if pattern.is_empty() {
        return Chain { seed, steps: Vec::new(), rank: 0.0 };
    }
    for slot in 0..length {
        let mode = pattern[slot % pattern.len()];
        let mut next: Vec<(f64, Vec<Step>)> = Vec::new();
        for (total, steps) in &beam {
            let last = steps.last().map_or(seed, |s| s.suggestion.row);
            let used = |row: Row| row == seed || steps.iter().any(|s| s.suggestion.row == row);
            let found = prepared.rank(last, mode, &|row| used(row) || exclude(row) || strays(row), BRANCH, variety);
            for suggestion in found {
                let mut grown = steps.clone();
                let rank = *total + suggestion.rank;
                grown.push(Step { mode, suggestion });
                next.push((rank, grown));
            }
        }
        if next.is_empty() {
            break;
        }
        next.sort_by(|a, b| b.0.total_cmp(&a.0));
        // One chain per ending: several orders of the same tracks must not fill the beam.
        let mut ends = HashSet::new();
        next.retain(|(_, steps)| steps.last().is_some_and(|s| ends.insert(s.suggestion.row)));
        next.truncate(BEAM);
        beam = next;
    }
    let (rank, steps) = beam.into_iter().next().unwrap_or((0.0, Vec::new()));
    Chain { seed, steps, rank }
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

    // ---------------------------------------------------------------- chains

    /// Ids 1 to 9 as rows 0 to 8: the seed, and what fits it as a transition or a double, or strays.
    fn chain_lib() -> Library {
        library_from(&[
            track(1, "Seed", "4A", 17400, "4A - Energy 6"),
            track(2, "A", "4A", 17400, "4A - Energy 6"),
            track(3, "B", "5A", 17500, "5A - Energy 6"),
            track(4, "C", "6A", 17400, "6A - Energy 6"),
            track(5, "X", "4A", 18400, ""),
            track(6, "Y", "4A", 19500, ""),
        ])
    }

    fn ids(chain: &Chain, lib: &Library) -> Vec<u64> {
        chain.steps.iter().map(|s| lib.ids[s.suggestion.row as usize]).collect()
    }

    #[test]
    fn a_pattern_is_read_as_transitions_and_doubles() {
        use Mode::{Double as D, Transition as T};
        assert_eq!(parse_pattern("DTD"), Some(vec![D, T, D]));
        assert_eq!(parse_pattern(" d, t - T>D "), Some(vec![D, T, T, D]));
        assert_eq!(parse_pattern(""), None);
        assert_eq!(parse_pattern("  ,"), None);
        assert_eq!(parse_pattern("DXT"), None, "an unknown letter is no pattern");
    }

    #[test]
    fn a_chain_fits_each_track_as_its_step_asks_and_repeats_the_pattern() {
        let lib = chain_lib();
        let pattern = parse_pattern("DT").unwrap();
        let chain = build_chain(&lib, 0, &pattern, 4, &|_| false, 0);
        assert_eq!(chain.steps.len(), 4);
        assert_eq!(chain.steps.iter().map(|s| s.mode).collect::<Vec<_>>(), [Mode::Double, Mode::Transition, Mode::Double, Mode::Transition]);
        let found = ids(&chain, &lib);
        let mut distinct = found.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), found.len(), "no track twice: {found:?}");
        assert!(!found.contains(&1), "the seed is not suggested after itself");
        // The first step is a double after the seed: a close key and tempo (A or B), never the energy boost (C) or X.
        assert!([2, 3].contains(&found[0]), "{found:?}");
    }

    #[test]
    fn a_pair_played_in_a_set_leads_the_chain() {
        use crate::testing::add_playlist;
        let mut lib = chain_lib();
        // B (row 2) came right after the seed (row 0) in a set. A fits the seed a little better on the numbers alone.
        let plain = build_chain(&lib, 0, &[Mode::Transition], 1, &|_| false, 0);
        assert_eq!(ids(&plain, &lib), [2]);
        add_playlist(&mut lib, "set", &[0, 2]);
        let taught = build_chain(&lib, 0, &[Mode::Transition], 1, &|_| false, 0);
        assert_eq!(ids(&taught, &lib), [3]);
        assert_eq!(taught.steps[0].suggestion.followed, 1);
    }

    #[test]
    fn a_chain_may_not_wander_far_from_the_seeds_tempo() {
        let lib = chain_lib();
        // Only X and Y are open: X is 5.4 % from the seed, Y 5.6 % from X but 10.8 % from the seed.
        let open = |row: Row| (1..=3).contains(&row);
        let chain = build_chain(&lib, 0, &[Mode::Transition], 3, &open, 0);
        assert_eq!(ids(&chain, &lib), [5], "Y strays too far and the chain ends");
    }

    #[test]
    fn a_chain_ends_when_nothing_fits_and_never_reuses_a_track() {
        let lib = chain_lib();
        let chain = build_chain(&lib, 0, &[Mode::Double], 6, &|_| false, 0);
        let found = ids(&chain, &lib);
        assert!(!found.is_empty() && found.len() < 6, "{found:?}");
        let mut distinct = found.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), found.len());
        assert!(chain.steps.iter().all(|s| s.mode == Mode::Double));
    }

    #[test]
    fn a_new_variety_reshuffles_equally_good_tracks() {
        let mut tracks = vec![track(1, "Seed", "4A", 17400, "")];
        let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
        for (i, name) in names.iter().enumerate() {
            tracks.push(track(10 + i as u64, name, "4A", 17400, ""));
        }
        let lib = library_from(&tracks);
        let first = |variety| ids(&build_chain(&lib, 0, &[Mode::Transition], 1, &|_| false, variety), &lib)[0];
        assert_eq!(first(0), first(0), "variety 0 is the plain, repeatable ranking");
        assert_eq!(first(7), first(7), "and so is any other");
        let picks: std::collections::HashSet<u64> = (1..=8).map(first).collect();
        assert!(picks.len() >= 2, "eight varieties picked one track: {picks:?}");
    }
}
