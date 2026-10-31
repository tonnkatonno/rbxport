//! The Next window: what to play after a playlist's last track.
//!
//! Fork only (not proposed for the core project). It opens like the Sync Manager does, at `index.html#next`, and asks
//! [`suggest_next`] for the playlist as it is now, its seed (the last track unless one is picked) and the tracks of the
//! library that fit it. Adding goes through the ordinary `add_tracks_to_playlist` edit and a preview through `preview_play`,
//! so the window needs no API of its own: the edit emits `library:changed` and the window asks again.

use std::collections::HashSet;
use std::sync::Arc;

use rbl_index::suggest::{self, Mode};
use rbl_index::{Library, Row};
use serde::Serialize;
use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::commands::{blocking, edit, Touched};
use crate::error::{AppError, AppResult, ErrorKind};
use crate::state::AppState;

pub const WINDOW: &str = "next";

const WIDTH: f64 = 1180.0;
const HEIGHT: f64 = 760.0;
/// The most suggestions one call returns.
const MAX_LIMIT: usize = 200;

/// Opens the window, or brings the open one to the front.
#[tauri::command]
// `async` for the reason `open_sync_window` is: a synchronous command runs on the main thread, which WebView2 needs to
// finish creating the webview.
#[allow(clippy::needless_pass_by_value)]
pub async fn open_next_window<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> AppResult<()> {
    if let Some(window) = app.get_webview_window(WINDOW) {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let builder = WebviewWindowBuilder::new(&app, WINDOW, WebviewUrl::App("index.html".into()))
        // Top frame only: on Windows wry runs it in every frame (see `report.rs`).
        .initialization_script("if (window === window.top && !location.hash) location.hash = '#next';")
        .title("Next");
    let builder = match crate::browser_args() {
        Some(args) => builder.additional_browser_args(&args),
        None => builder,
    };
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    builder
        .on_navigation(|url| {
            tracing::debug!(%url, "next window navigation");
            true
        })
        .on_page_load(|_, payload| {
            tracing::debug!(url = %payload.url(), event = ?payload.event(), "next window page load");
        })
        .inner_size(WIDTH, HEIGHT)
        .min_inner_size(760.0, 420.0)
        .visible(false)
        .resizable(true)
        .accept_first_mouse(true)
        .build()
        .map(|window| {
            let _ = window.hide_menu();
        })
        .map_err(|e| AppError::new(ErrorKind::Internal, format!("the Next window could not open: {e}")))
}

/// A track as the window lists it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NextTrackDto {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub key: String,
    pub bpm: f64,
    /// The Mixed In Key energy level read from the comment, 0 when there is none.
    pub energy: u32,
    pub length_sec: u32,
}

/// One suggestion with the reasons it fits.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NextItemDto {
    pub track: NextTrackDto,
    pub score: f64,
    pub key_distance: u32,
    pub transition: &'static str,
    pub bpm_diff_pct: f64,
    pub energy_diff: u32,
    /// How many times it came right after the seed in the library's own sets.
    pub followed: u32,
}

/// The playlist, the seed the suggestions follow and the suggestions.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NextDto {
    pub playlist_id: String,
    pub playlist_name: String,
    pub tracks: Vec<NextTrackDto>,
    pub seed: Option<NextTrackDto>,
    pub items: Vec<NextItemDto>,
}

fn track_dto(library: &Library, row: Row) -> NextTrackDto {
    let i = row as usize;
    NextTrackDto {
        id: library.ids[i].to_string(),
        title: library.title.get(i).to_string(),
        artist: library.artist_name(row).to_string(),
        key: library.key_name(row).to_string(),
        bpm: f64::from(library.bpm_x100.get(i).copied().unwrap_or(0)) / 100.0,
        energy: suggest::energy_of(library.comment.get(i)).unwrap_or(0),
        length_sec: library.length_sec.get(i).copied().unwrap_or(0),
    }
}

fn item_dto(library: &Library, s: &suggest::Suggestion) -> NextItemDto {
    NextItemDto {
        track: track_dto(library, s.row),
        score: (s.score * 10.0).round() / 10.0,
        key_distance: s.key_distance,
        transition: s.transition.name(),
        bpm_diff_pct: (s.bpm_diff_pct * 100.0).round() / 100.0,
        energy_diff: s.energy_diff,
        followed: s.followed,
    }
}

fn mode_of(name: &str) -> AppResult<Mode> {
    match name {
        "transition" => Ok(Mode::Transition),
        "double" => Ok(Mode::Double),
        other => Err(AppError::new(ErrorKind::Malformed, format!("unknown mode \"{other}\""))),
    }
}

/// A plain playlist's name and tracks in order. A folder or an intelligent playlist is not an order of the DJ's own, so it
/// is refused.
fn plain_playlist(library: &Library, playlist: &str) -> AppResult<(String, Vec<Row>)> {
    let playlists = library.playlists();
    let Some(index) = playlist.parse::<u64>().ok().and_then(|id| playlists.index_of(id)) else {
        return Err(AppError::new(ErrorKind::NotFound, "That playlist is not in the library."));
    };
    // 0 is a plain playlist; folders (1) and intelligent playlists (4) are not an order of the DJ's own.
    if playlists.attribute.get(index).copied().unwrap_or(0) != 0 {
        return Err(AppError::new(ErrorKind::Malformed, "Pick a plain playlist, not a folder or an intelligent one."));
    }
    Ok((playlists.names.get(index).to_string(), playlists.members.get(index).cloned().unwrap_or_default()))
}

/// The suggestions after `seed` (the playlist's last track when `None`), for a playlist.
///
/// The playlist's own tracks are never suggested.
#[tauri::command]
pub async fn suggest_next(
    state: State<'_, Arc<AppState>>,
    playlist: String,
    seed: Option<String>,
    mode: String,
    limit: Option<u32>,
) -> AppResult<NextDto> {
    let mode = mode_of(&mode)?;
    let limit = limit.map_or(60, |n| n as usize).clamp(1, MAX_LIMIT);
    let library = state.library()?;
    blocking("suggest_next", move || {
        let (name, members) = plain_playlist(&library, &playlist)?;
        let seed_row = match seed.as_deref() {
            Some(id) => id.parse::<u64>().ok().and_then(|n| library.row_of_id(n)),
            None => members.last().copied(),
        };
        let held: HashSet<Row> = members.iter().copied().collect();
        let items = seed_row
            .map(|row| {
                suggest::suggest(&library, row, mode, &|r| held.contains(&r), limit)
                    .iter()
                    .map(|s| item_dto(&library, s))
                    .collect()
            })
            .unwrap_or_default();
        Ok(NextDto {
            playlist_id: playlist,
            playlist_name: name,
            tracks: members.iter().map(|&r| track_dto(&library, r)).collect(),
            seed: seed_row.map(|r| track_dto(&library, r)),
            items,
        })
    })
    .await
}

/// One built track and how it is to be played with the one before it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NextStepDto {
    /// `T` (a transition) or `D` (a double drop).
    pub mode: &'static str,
    pub item: NextItemDto,
}

/// A chain built from a seed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NextBuildDto {
    pub seed: NextTrackDto,
    pub steps: Vec<NextStepDto>,
    /// How many tracks were asked for; fewer in `steps` means nothing fitted after the last.
    pub requested: u32,
}

/// The most tracks one build makes.
const MAX_BUILD: usize = 60;

/// Builds a chain of tracks after `seed` (the playlist's last track when `None`): the `i`-th fits the one before as the
/// `i`-th letter of `pattern` asks, `T` for a transition and `D` for a double drop, the pattern repeating to `length`.
///
/// The playlist's own tracks are left out, so the chain can be added to it. `variety` re-rolls the choice among equally good
/// tracks. Nothing is written: the window shows the chain and creates a playlist from it on request.
#[tauri::command]
pub async fn build_next(
    state: State<'_, Arc<AppState>>,
    playlist: String,
    seed: Option<String>,
    pattern: String,
    length: u32,
    variety: u32,
) -> AppResult<NextBuildDto> {
    let Some(modes) = suggest::parse_pattern(&pattern) else {
        return Err(AppError::new(
            ErrorKind::Malformed,
            "The pattern is made of T (a transition) and D (a double drop), one letter a track, for example DTD.",
        ));
    };
    let length = (length as usize).clamp(1, MAX_BUILD);
    let library = state.library()?;
    blocking("build_next", move || {
        let (_, members) = plain_playlist(&library, &playlist)?;
        let seed_row = match seed.as_deref() {
            Some(id) => id.parse::<u64>().ok().and_then(|n| library.row_of_id(n)),
            None => members.last().copied(),
        };
        let Some(seed_row) = seed_row else {
            return Err(AppError::new(ErrorKind::NotFound, "There is no track to start from: the playlist is empty."));
        };
        let held: HashSet<Row> = members.iter().copied().collect();
        let chain = suggest::build_chain(&library, seed_row, &modes, length, &|r| held.contains(&r), u64::from(variety));
        Ok(NextBuildDto {
            seed: track_dto(&library, seed_row),
            steps: chain
                .steps
                .iter()
                .map(|s| NextStepDto {
                    mode: if s.mode == Mode::Double { "D" } else { "T" },
                    item: item_dto(&library, &s.suggestion),
                })
                .collect(),
            requested: u32::try_from(length).unwrap_or(u32::MAX),
        })
    })
    .await
}

/// Creates a playlist at the top level holding `tracks` in order, as one edit.
///
/// Like creating a playlist in the browser, or adding tracks to one, it is not on the Undo stack: Delete Playlist (which is)
/// removes it. The name has to be free among the top-level playlists, which the library itself checks.
#[tauri::command]
pub async fn create_playlist_with_tracks<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Arc<AppState>>,
    name: String,
    tracks: Vec<String>,
) -> AppResult<u32> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::new(ErrorKind::Malformed, "Give the playlist a name."));
    }
    if tracks.is_empty() {
        return Err(AppError::new(ErrorKind::Malformed, "There are no tracks to put in the playlist."));
    }
    edit(app, state, "create_playlist_with_tracks", Touched::Playlists, move |w| {
        let id = w.create_playlist(&name, rbl_db::write::ROOT)?;
        w.add_tracks_allowing(&id, &tracks, false).map(|_| ())
    })
    .await
}
