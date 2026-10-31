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

use crate::commands::blocking;
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

fn mode_of(name: &str) -> AppResult<Mode> {
    match name {
        "transition" => Ok(Mode::Transition),
        "double" => Ok(Mode::Double),
        other => Err(AppError::new(ErrorKind::Malformed, format!("unknown mode \"{other}\""))),
    }
}

/// The suggestions after `seed` (the playlist's last track when `None`), for a playlist.
///
/// The playlist's own tracks are never suggested. A folder or an intelligent playlist has no order to continue, so it is
/// refused.
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
        let playlists = library.playlists();
        let Some(index) = playlist.parse::<u64>().ok().and_then(|id| playlists.index_of(id)) else {
            return Err(AppError::new(ErrorKind::NotFound, "That playlist is not in the library."));
        };
        // 0 is a plain playlist; folders (1) and intelligent playlists (4) are not an order of the DJ's own.
        if playlists.attribute.get(index).copied().unwrap_or(0) != 0 {
            return Err(AppError::new(ErrorKind::Malformed, "Pick a plain playlist, not a folder or an intelligent one."));
        }
        let name = playlists.names.get(index).to_string();
        let members: Vec<Row> = playlists.members.get(index).cloned().unwrap_or_default();
        drop(playlists);

        let seed_row = match seed.as_deref() {
            Some(id) => id.parse::<u64>().ok().and_then(|n| library.row_of_id(n)),
            None => members.last().copied(),
        };
        let held: HashSet<Row> = members.iter().copied().collect();
        let items = seed_row
            .map(|row| {
                suggest::suggest(&library, row, mode, &|r| held.contains(&r), limit)
                    .into_iter()
                    .map(|s| NextItemDto {
                        track: track_dto(&library, s.row),
                        score: (s.score * 10.0).round() / 10.0,
                        key_distance: s.key_distance,
                        transition: s.transition.name(),
                        bpm_diff_pct: (s.bpm_diff_pct * 100.0).round() / 100.0,
                        energy_diff: s.energy_diff,
                        followed: s.followed,
                    })
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
