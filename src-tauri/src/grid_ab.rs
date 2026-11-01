//! Grid A/B: which of two analyses of a track the library uses.
//!
//! Fork only (not proposed for the core project). A directory named by `RBXPORT_GRID_AB_DIR` keeps both analyses of each
//! track side by side, as the three files rekordbox makes (`DAT`, `EXT`, `2EX`):
//!
//! - `<dir>/rekordbox/<analysis path>`: the analysis rekordbox made;
//! - `<dir>/rbxport/<analysis path>`: the one rbxport made;
//! - `<dir>/index.json`: what a list needs without opening any of them (both tempos, where each grid starts, how far apart
//!   the grid lines are, and a suggestion from an outside check: see `docs/reference/grid-ab.md`), written by a script;
//! - `<dir>/active.json`: which of the two is in the library now, written here.
//!
//! Choosing a source copies its files over the track's own analysis files, through the same crash-safe file journal and
//! database write the beat grid editor uses, and gives any deck that holds the track the new grid, so the metronome moves at
//! once while it plays: the point is to hear one grid and then the other. Without the variable the feature is off, and the
//! installed library cannot be touched by it by accident.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::{blocking, reload, write_error};
use crate::error::{AppError, AppResult, ErrorKind};
use crate::next_window::{track_dto, NextTrackDto};
use crate::state::AppState;

pub const DIR_ENV: &str = "RBXPORT_GRID_AB_DIR";

/// A grid line this far from the other analysis's (in milliseconds), or a tempo this far apart (in hundredths of a BPM), is
/// something a metronome lets you hear; closer than that the two are one grid for a listener.
const AUDIBLE_PHASE_MS: f64 = 15.0;
const AUDIBLE_BPM_X100: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Rekordbox,
    Rbxport,
}

impl Source {
    fn parse(name: &str) -> AppResult<Self> {
        match name {
            "rekordbox" => Ok(Self::Rekordbox),
            "rbxport" => Ok(Self::Rbxport),
            other => Err(AppError::new(ErrorKind::Malformed, format!("unknown grid source \"{other}\""))),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Rekordbox => "rekordbox",
            Self::Rbxport => "rbxport",
        }
    }
}

/// What the index knows of one analysis.
#[derive(Debug, Clone, Deserialize)]
struct Side {
    bpm_x100: u32,
    first_ms: f64,
    /// 0 for an analysis that found no beats at all.
    #[serde(default)]
    beats: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct Entry {
    rekordbox: Side,
    rbxport: Side,
    /// One beat of the track, in milliseconds, at rekordbox's tempo.
    beat_ms: f64,
    /// `rbxport`, `rekordbox`, `either`, `same` or `ear`: what the outside check made of the pair.
    #[serde(default)]
    suggestion: Option<String>,
    /// How far each grid sits from the track's kick drums, in milliseconds, by that check.
    #[serde(default)]
    err_rb: Option<f64>,
    #[serde(default)]
    err_rbx: Option<f64>,
}

type Index = HashMap<String, Entry>;

fn dir() -> AppResult<PathBuf> {
    match std::env::var_os(DIR_ENV) {
        Some(d) if !d.is_empty() => Ok(PathBuf::from(d)),
        _ => Err(AppError::new(ErrorKind::NotFound, format!("Grid A/B is off: start rbxport with {DIR_ENV} set."))),
    }
}

fn read_index(dir: &Path) -> AppResult<Index> {
    let path = dir.join("index.json");
    let bytes = std::fs::read(&path).map_err(|e| {
        AppError::new(ErrorKind::NotFound, "The grid index could not be read.").with_detail(format!("{}: {e}", path.display()))
    })?;
    serde_json::from_slice(&bytes).map_err(|e| {
        AppError::new(ErrorKind::Malformed, "The grid index is not valid.").with_detail(format!("{}: {e}", path.display()))
    })
}

/// Which source each track has in the library now; a track not named has rbxport's, which is what a fresh store holds.
fn read_active(dir: &Path) -> HashMap<String, String> {
    std::fs::read(dir.join("active.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_active(dir: &Path, active: &HashMap<String, String>) -> AppResult<()> {
    let bytes = serde_json::to_vec(active).map_err(|e| AppError::internal(e.to_string()))?;
    crate::durable::write(&dir.join("active.json"), &bytes).map_err(|e| AppError::internal(e.to_string()))
}

fn active_of(active: &HashMap<String, String>, track: &str) -> Source {
    if active.get(track).map(String::as_str) == Some("rekordbox") { Source::Rekordbox } else { Source::Rbxport }
}

/// How far rbxport's grid lines sit from rekordbox's, folded to within half a beat (later is positive). Only meaningful when
/// the tempos agree, which is when it is looked at.
fn phase_ms(entry: &Entry) -> f64 {
    let beat = entry.beat_ms;
    if beat <= 0.0 {
        return 0.0;
    }
    let d = (entry.rbxport.first_ms - entry.rekordbox.first_ms).rem_euclid(beat);
    if d > beat / 2.0 { d - beat } else { d }
}

fn differs(entry: &Entry) -> bool {
    entry.rbxport.beats == 0
        || entry.rbxport.bpm_x100.abs_diff(entry.rekordbox.bpm_x100) >= AUDIBLE_BPM_X100
        || phase_ms(entry).abs() >= AUDIBLE_PHASE_MS
}

/// Where in a list a suggestion goes: the ones that need an ear first.
fn suggestion_rank(suggestion: Option<&str>) -> u8 {
    match suggestion {
        Some("ear") => 0,
        None => 1,
        Some("rbxport" | "rekordbox") => 2,
        _ => 3,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridAbRowDto {
    pub track: NextTrackDto,
    pub active: &'static str,
    pub rekordbox_bpm: f64,
    pub rbxport_bpm: f64,
    /// rbxport found no beats, so there is nothing to choose: the library row has no tempo until rekordbox's is put back.
    pub rbxport_empty: bool,
    pub phase_ms: f64,
    pub suggestion: Option<String>,
    pub err_rb: Option<f64>,
    pub err_rbx: Option<f64>,
    pub differs: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridAbStatusDto {
    pub enabled: bool,
    pub tracks: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridAbAppliedDto {
    pub applied: u32,
    pub skipped: u32,
    pub failed: Vec<String>,
}

fn row_dto(library: &rbl_index::Library, id: &str, entry: &Entry, active: Source) -> Option<GridAbRowDto> {
    let row = library.row_of(id)?;
    Some(GridAbRowDto {
        track: track_dto(library, row),
        active: active.name(),
        rekordbox_bpm: f64::from(entry.rekordbox.bpm_x100) / 100.0,
        rbxport_bpm: f64::from(entry.rbxport.bpm_x100) / 100.0,
        rbxport_empty: entry.rbxport.beats == 0,
        phase_ms: (phase_ms(entry) * 10.0).round() / 10.0,
        suggestion: entry.suggestion.clone(),
        err_rb: entry.err_rb.map(|e| (e * 10.0).round() / 10.0),
        err_rbx: entry.err_rbx.map(|e| (e * 10.0).round() / 10.0),
        differs: differs(entry),
    })
}

/// Whether the feature is on and how many tracks it holds.
#[tauri::command]
pub async fn grid_ab_status() -> AppResult<GridAbStatusDto> {
    let Ok(dir) = dir() else { return Ok(GridAbStatusDto { enabled: false, tracks: 0 }) };
    blocking("grid_ab_status", move || Ok(GridAbStatusDto { enabled: true, tracks: read_index(&dir)?.len() })).await
}

/// The tracks that have two analyses: those of one playlist, or all, and by default only where the two can be told apart.
#[tauri::command]
pub async fn grid_ab_list(
    state: State<'_, Arc<AppState>>,
    playlist: Option<String>,
    all: bool,
) -> AppResult<Vec<GridAbRowDto>> {
    let dir = dir()?;
    let library = state.library()?;
    blocking("grid_ab_list", move || {
        let index = read_index(&dir)?;
        let active = read_active(&dir);
        let only: Option<std::collections::HashSet<u64>> = match playlist.as_deref() {
            Some(p) if !p.is_empty() => {
                let playlists = library.playlists();
                let Some(i) = p.parse::<u64>().ok().and_then(|id| playlists.index_of(id)) else {
                    return Err(AppError::new(ErrorKind::NotFound, "That playlist is not in the library."));
                };
                Some(playlists.members.get(i).into_iter().flatten().map(|&r| library.ids[r as usize]).collect())
            }
            _ => None,
        };
        let mut rows: Vec<GridAbRowDto> = index
            .iter()
            .filter(|(id, _)| only.as_ref().is_none_or(|set| id.parse::<u64>().is_ok_and(|n| set.contains(&n))))
            .filter(|(_, entry)| all || differs(entry))
            .filter_map(|(id, entry)| row_dto(&library, id, entry, active_of(&active, id)))
            .collect();
        rows.sort_by(|a, b| {
            suggestion_rank(a.suggestion.as_deref())
                .cmp(&suggestion_rank(b.suggestion.as_deref()))
                .then_with(|| a.track.title.to_lowercase().cmp(&b.track.title.to_lowercase()))
        });
        Ok(rows)
    })
    .await
}

/// The beats a deck's metronome takes, from the file just written.
fn metronome_beats(dat: &Path) -> Vec<(u32, bool)> {
    crate::commands::read_beat_grid(dat).iter().map(|&(ms, number, _)| (ms, number == 1)).collect()
}

/// Puts one source's analysis in the library for a track. Returns the beats now in the track's grid and whether its tempo
/// column changed.
fn use_source(state: &AppState, track: &str, source: Source) -> AppResult<(Vec<(u32, bool)>, bool)> {
    let dir = dir()?;
    let index = read_index(&dir)?;
    let Some(entry) = index.get(track) else {
        return Err(AppError::new(ErrorKind::NotFound, "That track has no second analysis to choose."));
    };
    let _edit_guard = state.edit_gate.lock();
    let _files = state.analysis_write.lock();
    let location = state.location()?;
    if let Some(reason) = rbl_db::write_refusal_reason(location.is_real_install, rbl_db::test_mode(), rbl_db::is_rekordbox_running()) {
        return Err(AppError::new(ErrorKind::ReadOnly, reason));
    }
    if crate::grid::database_locked(&location, track)? {
        return Err(AppError::new(ErrorKind::ReadOnly, "The beat grid is locked. Unlock it to choose."));
    }
    crate::file_journal::recover(state.backup_dir(), &location)?;
    let library = state.library()?;
    let row = library.row_of(track).ok_or_else(|| AppError::new(ErrorKind::NotFound, "That track is no longer in the library."))?;
    let relative = state.analysis_path_of(&library, row as usize, track);
    if relative.is_empty() {
        return Err(AppError::new(ErrorKind::NotFound, "That track has not been analysed."));
    }
    let own = rbl_anlz::resolve(&location.share_root, &relative);
    let stored = rbl_anlz::resolve(&dir.join(source.name()), &relative);
    let mut files = Vec::new();
    for ext in ["DAT", "EXT", "2EX"] {
        match std::fs::read(rbl_anlz::sibling(&stored, ext)) {
            Ok(bytes) => files.push((rbl_anlz::sibling(&own, ext), bytes)),
            // The tempo and the beats are in the DAT: without it there is nothing to choose.
            Err(e) if ext == "DAT" => {
                return Err(AppError::new(ErrorKind::NotFound, "The stored analysis is missing.")
                    .with_detail(format!("{}: {e}", stored.display())));
            }
            Err(_) => {}
        }
    }
    let side = if source == Source::Rekordbox { &entry.rekordbox } else { &entry.rbxport };
    let bpm = side.bpm_x100;
    let old_bpm = library.bpm_x100.get(row as usize).copied().unwrap_or(0);
    let journal = crate::file_journal::FileJournal::prepare(state.backup_dir(), &location, track, bpm, None, true, &files)?;
    if let Err(e) = journal.publish() {
        journal.rollback()?;
        return Err(e);
    }
    if let Err(e) = state.write(|w| w.save_grid_revision(track, bpm)) {
        journal.reconcile(&location)?;
        return Err(write_error(e));
    }
    journal.commit()?;
    let dir_for_active = dir;
    let mut active = read_active(&dir_for_active);
    active.insert(track.to_owned(), source.name().to_owned());
    write_active(&dir_for_active, &active)?;
    Ok((metronome_beats(&own), bpm != old_bpm))
}

/// Chooses which analysis a track uses, and gives the deck it is on (if any) the new grid.
#[tauri::command]
pub async fn grid_ab_use<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Arc<AppState>>,
    player: State<'_, Arc<crate::player::Player>>,
    track: String,
    source: String,
) -> AppResult<GridAbRowDto> {
    let source = Source::parse(&source)?;
    let (beats, bpm_changed) = {
        let state = Arc::clone(&state);
        let track = track.clone();
        blocking("grid_ab_use", move || use_source(&state, &track, source)).await?
    };
    // The deck plays the grid it was loaded with: every deck holding the track is given this one.
    if let Some(engine) = player.opened() {
        for (deck, loaded) in player.loaded_tracks.lock().iter() {
            if *loaded == track {
                engine.set_metronome_grid(*deck, &beats);
            }
        }
    }
    let _ = tauri::Emitter::emit(&app, "grid:changed", &track);
    let state = Arc::clone(&state);
    if bpm_changed {
        reload(app, Arc::clone(&state)).await?;
    }
    let dir = dir()?;
    let index = read_index(&dir)?;
    let entry = index.get(&track).ok_or_else(|| AppError::new(ErrorKind::NotFound, "That track has no second analysis."))?;
    // After the reload: the row's tempo is the one just chosen.
    let library = state.library()?;
    row_dto(&library, &track, entry, source)
        .ok_or_else(|| AppError::new(ErrorKind::NotFound, "That track is no longer in the library."))
}

/// Applies the outside check's suggestions for the tracks where the two can be told apart: `rbxport` or `rekordbox` where
/// the check was sure, nothing where it was not (`either`, `same`, `ear`, or no suggestion).
#[tauri::command]
pub async fn grid_ab_apply<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Arc<AppState>>,
    playlist: Option<String>,
) -> AppResult<GridAbAppliedDto> {
    let dir = dir()?;
    let library = state.library()?;
    let only: Option<std::collections::HashSet<u64>> = match playlist.as_deref() {
        Some(p) if !p.is_empty() => {
            let playlists = library.playlists();
            let Some(i) = p.parse::<u64>().ok().and_then(|id| playlists.index_of(id)) else {
                return Err(AppError::new(ErrorKind::NotFound, "That playlist is not in the library."));
            };
            Some(playlists.members.get(i).into_iter().flatten().map(|&r| library.ids[r as usize]).collect())
        }
        _ => None,
    };
    let worker = Arc::clone(&state);
    let outcome = blocking("grid_ab_apply", move || {
        let index = read_index(&dir)?;
        let active = read_active(&dir);
        let mut out = GridAbAppliedDto { applied: 0, skipped: 0, failed: Vec::new() };
        let mut wanted: Vec<(&String, Source)> = Vec::new();
        for (id, entry) in &index {
            if only.as_ref().is_some_and(|set| !id.parse::<u64>().is_ok_and(|n| set.contains(&n))) || !differs(entry) {
                continue;
            }
            let target = match entry.suggestion.as_deref() {
                Some("rbxport") if entry.rbxport.beats > 0 => Source::Rbxport,
                Some("rekordbox") => Source::Rekordbox,
                _ => {
                    out.skipped += 1;
                    continue;
                }
            };
            if active_of(&active, id) == target {
                out.skipped += 1;
            } else {
                wanted.push((id, target));
            }
        }
        for (id, target) in wanted {
            match use_source(&worker, id, target) {
                Ok(_) => out.applied += 1,
                Err(e) => out.failed.push(format!("{id}: {}", e.message)),
            }
        }
        Ok(out)
    })
    .await?;
    if outcome.applied > 0 {
        reload(app, Arc::clone(&state)).await?;
    }
    Ok(outcome)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn entry(old_first: f64, new_first: f64, old_bpm: u32, new_bpm: u32, new_beats: u32) -> Entry {
        Entry {
            rekordbox: Side { bpm_x100: old_bpm, first_ms: old_first, beats: 700 },
            rbxport: Side { bpm_x100: new_bpm, first_ms: new_first, beats: new_beats },
            beat_ms: 344.83,
            suggestion: None,
            err_rb: None,
            err_rbx: None,
        }
    }

    #[test]
    fn the_phase_folds_to_within_half_a_beat() {
        assert!((phase_ms(&entry(24.0, 24.0, 17400, 17400, 700))).abs() < 1e-9);
        // Later is positive.
        assert!((phase_ms(&entry(24.0, 174.0, 17400, 17400, 700)) - 150.0).abs() < 1e-9);
        // Over half a beat (172.4 ms) later is the same lines as a little earlier.
        assert!((phase_ms(&entry(24.0, 24.0 + 173.0, 17400, 17400, 700)) - (173.0 - 344.83)).abs() < 1e-9);
        assert!((phase_ms(&entry(24.0, 24.0 + 200.0, 17400, 17400, 700)) - (200.0 - 344.83)).abs() < 1e-9);
        // Whole beats apart are no phase at all.
        assert!(phase_ms(&entry(24.0, 24.0 + 3.0 * 344.83, 17400, 17400, 700)).abs() < 1e-6);
        // A grid that starts earlier is negative.
        assert!((phase_ms(&entry(174.0, 24.0, 17400, 17400, 700)) + 150.0).abs() < 1e-9);
    }

    #[test]
    fn two_grids_differ_when_a_metronome_would_tell_them_apart() {
        assert!(!differs(&entry(24.0, 24.0, 17400, 17400, 700)), "identical");
        assert!(!differs(&entry(24.0, 30.0, 17400, 17400, 700)), "6 ms is one grid to a listener");
        assert!(!differs(&entry(24.0, 24.0, 17400, 17410, 700)), "0.1 BPM is too little");
        assert!(differs(&entry(24.0, 60.0, 17400, 17400, 700)), "36 ms is audible");
        assert!(differs(&entry(24.0, 24.0, 17400, 17420, 700)), "0.2 BPM drifts");
        assert!(differs(&entry(24.0, 24.0, 17400, 0, 0)), "an empty rbxport grid always needs a decision");
    }

    #[test]
    fn a_track_not_named_in_the_active_file_has_rbxports_grid() {
        let mut active = HashMap::new();
        assert_eq!(active_of(&active, "1"), Source::Rbxport);
        active.insert("1".to_owned(), "rekordbox".to_owned());
        active.insert("2".to_owned(), "rbxport".to_owned());
        assert_eq!((active_of(&active, "1"), active_of(&active, "2"), active_of(&active, "3")), (Source::Rekordbox, Source::Rbxport, Source::Rbxport));
    }

    #[test]
    fn sources_are_parsed_by_name_and_nothing_else() {
        assert_eq!(Source::parse("rekordbox").unwrap(), Source::Rekordbox);
        assert_eq!(Source::parse("rbxport").unwrap(), Source::Rbxport);
        assert!(Source::parse("Rekordbox").is_err());
        assert!(Source::parse("").is_err());
        assert_eq!(Source::Rekordbox.name(), "rekordbox");
    }

    #[test]
    fn the_index_reads_with_only_the_fields_a_script_must_write() {
        let json = r#"{"7":{"rekordbox":{"bpm_x100":17400,"first_ms":24.0},"rbxport":{"bpm_x100":17400,"first_ms":24.0,"beats":700},"beat_ms":344.8}}"#;
        let index: Index = serde_json::from_str(json).unwrap();
        let e = &index["7"];
        assert_eq!((e.rekordbox.beats, e.rbxport.beats, e.suggestion.is_none(), e.err_rb.is_none()), (0, 700, true, true));
    }

    #[test]
    fn the_ones_that_need_an_ear_come_first() {
        let mut v = vec![Some("rbxport"), None, Some("either"), Some("ear"), Some("rekordbox")];
        v.sort_by_key(|s| suggestion_rank(*s));
        assert_eq!(v, vec![Some("ear"), None, Some("rbxport"), Some("rekordbox"), Some("either")]);
    }
}
