//! A small local API for another program on this machine: read the playlists, and add tracks to one.
//!
//! It exists so a companion app (the double-drop searcher) can put a track into the playlist being built in rbxport and
//! see it appear at once, with the library held open by rbxport rather than rekordbox. It is NOT part of the core
//! project: it lives in this fork only, and is off unless asked for.
//!
//! - `RBXPORT_API_PORT=<port>` turns it on. It listens on `127.0.0.1` only.
//! - Every request needs `Authorization: Bearer <token>`. The token is made at start and written to
//!   `RBXPORT_API_TOKEN_FILE` (default `<local data dir>/rbxport/api-token`), so a program that can read the user's
//!   files can use the API and a web page cannot: a custom header makes a browser ask first, and no CORS answer is given.
//!   The `Host` header must name this listener, which stops DNS-rebinding.
//! - Reading is always allowed. Writing needs `RBXPORT_API_WRITE=1` as well, because Library Protection (Preferences) is a
//!   choice only the interface sees. A write goes through the same path as the interface's own (`commands::edit`): the
//!   index refreshes, `library:changed` is emitted, and a player on LINK sees the change when it browses.
//!
//! Routes (JSON in and out):
//!
//! - `GET  /v1/status`
//! - `GET  /v1/playlists`: every playlist, folder and intelligent playlist, with its parent and track count.
//! - `GET  /v1/playlists/{id}`: one playlist with its tracks in order (`id`, `title`).
//! - `POST /v1/playlists/{id}/tracks` with `{"tracks": ["<content id>", ...], "allow_duplicates": false}`.
//! - `POST /v1/playlists` with `{"name": "...", "tracks": ["<content id>", ...]}`: a new top-level playlist holding the tracks in
//!   order, as one edit. Answers with its `id`. A name already taken is refused (409).
//!   `"allow_duplicates": true` keeps a track listed more than once.
//! - `POST /v1/tracks/import` with `{"path": "D:/.../file.flac"}`: adds one audio file to the library (or finds it there) and answers its id.
//! - `GET  /v1/tracks/{id}/cues`: the track cues (memory and hot, hot ones with their letter).
//! - `POST /v1/tracks/{id}/hotcues` with `{"cues": [{"slot": "B", "ms": 88275}, ...]}`, slots A to H: adds hot cues. A slot that is
//!   already taken is skipped and reported, never replaced.
//! - `POST /v1/hotcues/arrange` with `{"tracks": [{"id": "123", "delete": ["A"], "move": [["B", "A"]]}, ...]}`, 1 to 500 tracks:
//!   deletes, then moves hot cues to free slots, in one write. Letters name the slots as they were; a move onto a taken slot is
//!   skipped and reported.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// Names the port; the API is off without it.
pub const PORT_ENV: &str = "RBXPORT_API_PORT";
/// `1` lets the API change the library. Without it every write is refused.
pub const WRITE_ENV: &str = "RBXPORT_API_WRITE";
/// Where the token is written.
pub const TOKEN_FILE_ENV: &str = "RBXPORT_API_TOKEN_FILE";

const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Starts listening, on a thread of its own, when a port is asked for.
pub fn start(app: &AppHandle) {
    let Some(port) = std::env::var(PORT_ENV).ok().and_then(|v| v.parse::<u16>().ok()) else { return };
    let allow_write = std::env::var(WRITE_ENV).is_ok_and(|v| v == "1");
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let token_file = token_path();
    if let Err(e) = write_token(&token_file, &token) {
        tracing::error!(error = %e, file = %token_file.display(), "the local API's token could not be written; the API is off");
        return;
    }
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(port, error = %e, "the local API could not open its port");
            return;
        }
    };
    tracing::info!(port, writes = allow_write, token_file = %token_file.display(), "local API listening on 127.0.0.1");
    let app = app.clone();
    let spawned = std::thread::Builder::new().name("local-api".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let (app, token) = (app.clone(), token.clone());
            let _ = std::thread::Builder::new().name("local-api-conn".into()).spawn(move || {
                handle(stream, &app, &token, port, allow_write);
            });
        }
    });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "the local API's thread could not start");
    }
}

fn token_path() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os(TOKEN_FILE_ENV) {
        return path.into();
    }
    dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("rbxport").join("api-token")
}

fn write_token(path: &std::path::Path, token: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, token)
}

// ---------------------------------------------------------------------------- HTTP

#[derive(Debug, PartialEq, Eq)]
struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

/// Reads one request: the head up to the blank line, then `Content-Length` bytes.
fn read_request(stream: &mut impl Read) -> Result<Request, (u16, &'static str)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            if at > MAX_HEAD {
                return Err((431, "request head too large"));
            }
            break at;
        }
        if buf.len() > MAX_HEAD {
            return Err((431, "request head too large"));
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err((400, "incomplete request")),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let (head, rest) = (&buf[..head_end], &buf[head_end + 4..]);
    let head = std::str::from_utf8(head).map_err(|_| (400, "request head is not UTF-8"))?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(target)) = (first.next(), first.next()) else { return Err((400, "bad request line")) };
    let path = target.split('?').next().unwrap_or("").to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .map_or(Ok(0), |(_, v)| v.parse::<usize>())
        .map_err(|_| (400, "bad content-length"))?;
    if length > MAX_BODY {
        return Err((413, "body too large"));
    }
    let mut body = rest.to_vec();
    while body.len() < length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return Err((400, "incomplete body")),
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    body.truncate(length);
    Ok(Request { method: method.to_owned(), path, headers, body })
}

/// `Authorization: Bearer <token>`, compared without stopping at the first difference.
fn authorized(request: &Request, token: &str) -> bool {
    let Some(value) = request.header("authorization") else { return false };
    let Some(given) = value.strip_prefix("Bearer ") else { return false };
    let (a, b) = (given.as_bytes(), token.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// The `Host` must name this listener (a rebinding page reaches it under its own name).
fn host_ok(request: &Request, port: u16) -> bool {
    request
        .header("host")
        .is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"))
}

fn respond(stream: &mut TcpStream, status: u16, body: &Value) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let payload = serde_json::to_vec(body).unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&payload);
    let _ = stream.flush();
}

fn handle(mut stream: TcpStream, app: &AppHandle, token: &str, port: u16, allow_write: bool) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err((status, message)) => return respond(&mut stream, status, &json!({ "error": message })),
    };
    if !host_ok(&request, port) {
        return respond(&mut stream, 403, &json!({ "error": "wrong Host" }));
    }
    if !authorized(&request, token) {
        return respond(&mut stream, 401, &json!({ "error": "missing or wrong bearer token" }));
    }
    let (status, body) = route(&request, app, allow_write);
    respond(&mut stream, status, &body);
}

// ---------------------------------------------------------------------------- routes

type Reply = (u16, Value);

fn fail(status: u16, message: impl Into<String>) -> Reply {
    (status, json!({ "error": message.into() }))
}

fn route(request: &Request, app: &AppHandle, allow_write: bool) -> Reply {
    let segments: Vec<&str> = request.path.trim_matches('/').split('/').collect();
    let state = app.state::<Arc<AppState>>();
    match (request.method.as_str(), segments.as_slice()) {
        ("GET", ["v1", "status"]) => status(&state, allow_write),
        ("GET", ["v1", "playlists"]) => playlists(&state),
        ("GET", ["v1", "playlists", id]) => playlist(&state, id),
        ("POST", ["v1", "playlists", id, "tracks"]) => add_tracks(app, &state, id, &request.body, allow_write),
        ("POST", ["v1", "playlists"]) => create_playlist(app, &state, &request.body, allow_write),
        ("POST", ["v1", "tracks", "import"]) => import_track(app, &request.body, allow_write),
        ("GET", ["v1", "tracks", id, "cues"]) => track_cues(&state, id),
        ("POST", ["v1", "tracks", id, "hotcues"]) => add_hot_cues(app, &state, id, &request.body, allow_write),
        ("POST", ["v1", "hotcues", "arrange"]) => arrange_hot_cues(app, &request.body, allow_write),
        (_, ["v1", ..]) => fail(404, "no such route, or the method is not allowed on it"),
        _ => fail(404, "no such route"),
    }
}

fn status(state: &AppState, allow_write: bool) -> Reply {
    let library = state.library().ok();
    (
        200,
        json!({
            "app": "rbxport",
            "version": env!("CARGO_PKG_VERSION"),
            "ready": library.is_some(),
            "tracks": library.as_ref().map(|l| l.len()),
            "playlists": library.as_ref().map(|l| l.playlists().ids.len()),
            "writes": allow_write,
        }),
    )
}

fn kind_of(attribute: u8) -> &'static str {
    match attribute {
        rbl_index::ATTRIBUTE_FOLDER => "folder",
        rbl_index::ATTRIBUTE_SMART => "smart",
        _ => "playlist",
    }
}

fn playlists(state: &AppState) -> Reply {
    let Ok(library) = state.library() else { return fail(503, "the library has not finished loading yet") };
    let p = library.playlists();
    let list: Vec<Value> = (0..p.ids.len())
        .map(|i| {
            let parent = p.parent.get(i).and_then(|&x| p.ids.get(x as usize)).map(ToString::to_string);
            json!({
                "id": p.ids[i].to_string(),
                "name": p.names.get(i),
                "parent": parent,
                "kind": kind_of(p.attribute.get(i).copied().unwrap_or(0)),
                "tracks": p.members.get(i).map_or(0, Vec::len),
            })
        })
        .collect();
    (200, json!({ "playlists": list }))
}

fn find_playlist(p: &rbl_index::Playlists, id: &str) -> Option<usize> {
    let id: u64 = id.parse().ok()?;
    p.ids.iter().position(|&x| x == id)
}

fn playlist(state: &AppState, id: &str) -> Reply {
    let Ok(library) = state.library() else { return fail(503, "the library has not finished loading yet") };
    let p = library.playlists();
    let Some(i) = find_playlist(&p, id) else { return fail(404, "no such playlist") };
    let kind = kind_of(p.attribute.get(i).copied().unwrap_or(0));
    let tracks: Vec<Value> = p
        .members
        .get(i)
        .map(|rows| {
            rows.iter()
                .map(|&row| json!({ "id": library.ids[row as usize].to_string(), "title": library.title.get(row as usize) }))
                .collect()
        })
        .unwrap_or_default();
    (200, json!({ "id": id, "name": p.names.get(i), "kind": kind, "tracks": tracks }))
}

/// A content id out of the JSON a client sent: a string or a number.
fn content_id(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => Some(s.clone()),
        Value::Number(n) => n.as_u64().map(|n| n.to_string()),
        _ => None,
    }
}

/// `{"name": "...", "tracks": ["id", ...]}`: the new playlist name (trimmed, 1 to 200 characters) and its tracks, 1 to 500.
fn parse_new_playlist(body: &[u8]) -> Result<(String, Vec<String>), String> {
    let parsed: Value = serde_json::from_slice(body).map_err(|_| "the body is not JSON".to_owned())?;
    let name = parsed.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if name.is_empty() || name.chars().count() > 200 || name.chars().any(char::is_control) {
        return Err("\"name\" must be 1 to 200 characters, with no control characters".to_owned());
    }
    let wanted = parsed.get("tracks").and_then(Value::as_array).ok_or_else(|| "\"tracks\" must be a list".to_owned())?;
    let ids: Option<Vec<String>> = wanted.iter().map(content_id).collect();
    let ids = ids.ok_or_else(|| "every track must be a content id (digits)".to_owned())?;
    if ids.is_empty() || ids.len() > 500 {
        return Err("1 to 500 tracks".to_owned());
    }
    Ok((name.to_owned(), ids))
}

fn create_playlist(app: &AppHandle, state: &AppState, body: &[u8], allow_write: bool) -> Reply {
    if !allow_write {
        return fail(403, format!("writes are off; start rbxport with {WRITE_ENV}=1"));
    }
    let (name, ids) = match parse_new_playlist(body) {
        Ok(parsed) => parsed,
        Err(message) => return fail(400, message),
    };
    let allow_duplicates = serde_json::from_slice::<Value>(body).ok().and_then(|v| v.get("allow_duplicates").and_then(Value::as_bool)).unwrap_or(false);
    let Ok(library) = state.library() else { return fail(503, "the library has not finished loading yet") };
    let unknown: Vec<&String> = ids
        .iter()
        .filter(|t| t.parse::<u64>().ok().and_then(|n| library.row_of_id(n)).is_none())
        .collect();
    if !unknown.is_empty() {
        return (404, json!({ "error": "unknown track ids", "unknown": unknown }));
    }
    // The library does not refuse a second playlist with the same name, and a retried request would silently make a copy: refuse it here.
    {
        let p = library.playlists();
        let taken = (0..p.ids.len()).any(|i| {
            let top_level = p.parent.get(i).and_then(|&x| p.ids.get(x as usize)).is_none();
            top_level && p.names.get(i).trim().eq_ignore_ascii_case(&name)
        });
        if taken {
            return fail(409, format!("a top-level playlist called \"{name}\" already exists"));
        }
    }
    match tauri::async_runtime::block_on(crate::next_window::create_playlist_made(app.clone(), app.state(), name.clone(), ids.clone(), allow_duplicates)) {
        Ok((_generation, id)) => {
            tracing::info!(playlist = %id, tracks = ids.len(), "local API created a playlist");
            (200, json!({ "ok": true, "id": id, "name": name, "tracks": ids.len() }))
        }
        Err(e) => (409, json!({ "error": e.message })),
    }
}

/// `{"cues": [{"slot": "B", "ms": 88275}, ...]}`: hot cue letters A to H with positions in ms (up to 24 h), 1 to 16 of them.
fn parse_hot_cues(body: &[u8]) -> Result<Vec<(char, u32)>, String> {
    let parsed: Value = serde_json::from_slice(body).map_err(|_| "the body is not JSON".to_owned())?;
    let list = parsed.get("cues").and_then(Value::as_array).ok_or_else(|| "\"cues\" must be a list".to_owned())?;
    if list.is_empty() || list.len() > 16 {
        return Err("1 to 16 cues per request".to_owned());
    }
    list.iter()
        .map(|cue| {
            let slot = cue
                .get("slot")
                .and_then(Value::as_str)
                .filter(|s| s.chars().count() == 1)
                .and_then(|s| s.chars().next())
                .map(|c| c.to_ascii_uppercase())
                .filter(|c| ('A'..='H').contains(c))
                .ok_or_else(|| "each cue needs a \"slot\" from A to H".to_owned())?;
            let ms = cue
                .get("ms")
                .and_then(Value::as_u64)
                .filter(|&ms| ms <= 86_400_000)
                .and_then(|ms| u32::try_from(ms).ok())
                .ok_or_else(|| "each cue needs \"ms\", a whole number of milliseconds up to 24 hours".to_owned())?;
            Ok((slot, ms))
        })
        .collect()
}

fn add_hot_cues(app: &AppHandle, state: &AppState, id: &str, body: &[u8], allow_write: bool) -> Reply {
    if !allow_write {
        return fail(403, format!("writes are off; start rbxport with {WRITE_ENV}=1"));
    }
    let wanted = match parse_hot_cues(body) {
        Ok(wanted) => wanted,
        Err(message) => return fail(400, message),
    };
    let Ok(library) = state.library() else { return fail(503, "the library has not finished loading yet") };
    if id.parse::<u64>().ok().and_then(|n| library.row_of_id(n)).is_none() {
        return fail(404, "no such track");
    }
    match tauri::async_runtime::block_on(crate::cues::add_hot_cues(app.clone(), id.to_owned(), wanted)) {
        Ok(done) => {
            tracing::info!(track = id, added = done.added.len(), skipped = done.skipped.len(), "local API added hot cues");
            let added: Vec<Value> = done.added.iter().map(|(slot, ms, cue)| json!({ "slot": slot.to_string(), "ms": ms, "id": cue })).collect();
            let skipped: Vec<Value> = done.skipped.iter().map(|(slot, why)| json!({ "slot": slot.to_string(), "reason": why })).collect();
            (200, json!({ "ok": true, "added": added, "skipped": skipped }))
        }
        Err(e) => (409, json!({ "error": e.message })),
    }
}

fn hot_letter(v: &Value) -> Option<char> {
    let mut chars = v.as_str()?.chars();
    let c = chars.next()?.to_ascii_uppercase();
    (chars.next().is_none() && ('A'..='H').contains(&c)).then_some(c)
}

/// `{"tracks": [{"id": "123", "delete": ["A"], "move": [["B", "A"], ["C", "B"]]}, ...]}`, 1 to 500 tracks.
fn parse_arrangements(body: &[u8]) -> Result<Vec<crate::cues::Arrangement>, String> {
    let parsed: Value = serde_json::from_slice(body).map_err(|_| "the body is not JSON".to_owned())?;
    let list = parsed.get("tracks").and_then(Value::as_array).ok_or_else(|| "\"tracks\" must be a list".to_owned())?;
    if list.is_empty() || list.len() > 500 {
        return Err("1 to 500 tracks per request".to_owned());
    }
    list.iter()
        .map(|t| {
            let track = t.get("id").and_then(content_id).ok_or_else(|| "each track needs an \"id\" (digits)".to_owned())?;
            let delete = match t.get("delete").and_then(Value::as_array) {
                None => Vec::new(),
                Some(l) => l.iter().map(|v| hot_letter(v).ok_or_else(|| "\"delete\" holds letters A to H".to_owned())).collect::<Result<_, _>>()?,
            };
            let moves = match t.get("move").and_then(Value::as_array) {
                None => Vec::new(),
                Some(l) => l
                    .iter()
                    .map(|m| match m.as_array().map(Vec::as_slice) {
                        Some([from, to]) => hot_letter(from).zip(hot_letter(to)).ok_or_else(|| "a move is two letters A to H".to_owned()),
                        _ => Err("each move is [\"from\", \"to\"]".to_owned()),
                    })
                    .collect::<Result<_, _>>()?,
            };
            Ok(crate::cues::Arrangement { track, delete, moves })
        })
        .collect()
}

fn arrange_hot_cues(app: &AppHandle, body: &[u8], allow_write: bool) -> Reply {
    if !allow_write {
        return fail(403, format!("writes are off; start rbxport with {WRITE_ENV}=1"));
    }
    let plans = match parse_arrangements(body) {
        Ok(plans) => plans,
        Err(message) => return fail(400, message),
    };
    match tauri::async_runtime::block_on(crate::cues::arrange_hot_cues(app.clone(), plans)) {
        Ok(done) => {
            let changed = done.iter().filter(|o| !o.deleted.is_empty() || !o.moved.is_empty()).count();
            tracing::info!(tracks = done.len(), changed, "local API rearranged hot cues");
            let tracks: Vec<Value> = done
                .iter()
                .map(|o| {
                    json!({
                        "id": o.track,
                        "deleted": o.deleted.iter().map(char::to_string).collect::<Vec<_>>(),
                        "moved": o.moved.iter().map(|(f, t)| [f.to_string(), t.to_string()]).collect::<Vec<_>>(),
                        "skipped": o.skipped,
                    })
                })
                .collect();
            (200, json!({ "ok": true, "tracks": tracks }))
        }
        Err(e) => (409, json!({ "error": e.message })),
    }
}

/// `{"path": "..."}`: one existing audio file, by absolute path (no folder: a folder would import everything under it).
fn parse_import(body: &[u8]) -> Result<std::path::PathBuf, String> {
    let parsed: Value = serde_json::from_slice(body).map_err(|_| "the body is not JSON".to_owned())?;
    let path = parsed.get("path").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    let path = std::path::PathBuf::from(path);
    if !path.is_absolute() {
        return Err("\"path\" must be an absolute file path".to_owned());
    }
    Ok(path)
}

fn import_track(app: &AppHandle, body: &[u8], allow_write: bool) -> Reply {
    if !allow_write {
        return fail(403, format!("writes are off; start rbxport with {WRITE_ENV}=1"));
    }
    let path = match parse_import(body) {
        Ok(path) => path,
        Err(message) => return fail(400, message),
    };
    if !path.is_file() {
        return fail(404, "no such file (one audio file, not a folder)");
    }
    let wanted = path.to_string_lossy().into_owned();
    match tauri::async_runtime::block_on(crate::commands::import_files(app.clone(), app.state(), vec![wanted])) {
        Ok(report) => {
            if let Some(t) = report.tracks.first() {
                tracing::info!(track = %t.id, "local API imported a file");
                (200, json!({ "ok": true, "id": t.id, "added": true }))
            } else if let Some(t) = report.existing.first() {
                (200, json!({ "ok": true, "id": t.id, "added": false }))
            } else {
                fail(409, report.skipped.first().cloned().unwrap_or_else(|| "the file was not imported".to_owned()))
            }
        }
        Err(e) => (409, json!({ "error": e.message })),
    }
}

fn track_cues(state: &AppState, id: &str) -> Reply {
    let Ok(library) = state.library() else { return fail(503, "the library has not finished loading yet") };
    let Some(row) = id.parse::<u64>().ok().and_then(|n| library.row_of_id(n)) else { return fail(404, "no such track") };
    let cues: Vec<Value> = library
        .cues_of(row)
        .iter()
        .map(|c| json!({ "kind": c.kind, "slot": c.hot_letter().map(|l| l.to_string()), "ms": c.position_ms, "loop_out_ms": c.out_ms }))
        .collect();
    (200, json!({ "track": id, "cues": cues }))
}

fn add_tracks(app: &AppHandle, state: &AppState, id: &str, body: &[u8], allow_write: bool) -> Reply {
    if !allow_write {
        return fail(403, format!("writes are off; start rbxport with {WRITE_ENV}=1"));
    }
    let Ok(parsed) = serde_json::from_slice::<Value>(body) else { return fail(400, "the body is not JSON") };
    let Some(wanted) = parsed.get("tracks").and_then(Value::as_array) else { return fail(400, "\"tracks\" must be a list") };
    let allow_duplicates = parsed.get("allow_duplicates").and_then(Value::as_bool).unwrap_or(false);
    let Some(ids): Option<Vec<String>> = wanted.iter().map(content_id).collect() else {
        return fail(400, "every track must be a content id (digits)");
    };
    if ids.is_empty() {
        return fail(400, "no tracks given");
    }
    let Ok(library) = state.library() else { return fail(503, "the library has not finished loading yet") };

    let (playlist_id, held) = {
        let p = library.playlists();
        let Some(i) = find_playlist(&p, id) else { return fail(404, "no such playlist") };
        if p.attribute.get(i).copied().unwrap_or(0) != 0 {
            return fail(409, "that is a folder or an intelligent playlist, not a plain playlist");
        }
        let held: std::collections::HashSet<u64> = p
            .members
            .get(i)
            .map(|rows| rows.iter().map(|&row| library.ids[row as usize]).collect())
            .unwrap_or_default();
        (p.ids[i].to_string(), held)
    };
    let unknown: Vec<&String> = ids
        .iter()
        .filter(|t| t.parse::<u64>().ok().and_then(|n| library.row_of_id(n)).is_none())
        .collect();
    if !unknown.is_empty() {
        return (404, json!({ "error": "unknown track ids", "unknown": unknown }));
    }
    let already: Vec<&String> = ids.iter().filter(|t| t.parse::<u64>().is_ok_and(|n| held.contains(&n))).collect();
    let to_add: Vec<String> = if allow_duplicates {
        ids.clone()
    } else {
        ids.iter().filter(|t| !already.contains(t)).cloned().collect()
    };
    if to_add.is_empty() {
        return (200, json!({ "ok": true, "added": 0, "already_in_playlist": already.len() }));
    }
    let added = to_add.len();
    match tauri::async_runtime::block_on(crate::commands::add_tracks_to_playlist(
        app.clone(),
        app.state(),
        playlist_id,
        to_add,
        Some(allow_duplicates),
    )) {
        Ok(generation) => {
            tracing::info!(playlist = id, added, "local API added tracks to a playlist");
            (200, json!({ "ok": true, "added": added, "already_in_playlist": already.len(), "generation": generation }))
        }
        Err(e) => (409, json!({ "error": e.message })),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn an_arrangement_request_names_tracks_and_letters() {
        let got = parse_arrangements(br#"{"tracks": [{"id": "12", "delete": ["a"], "move": [["B", "A"], ["C", "B"]]}, {"id": 7}]}"#).unwrap();
        assert_eq!(got[0].track, "12");
        assert_eq!(got[0].delete, vec!['A']);
        assert_eq!(got[0].moves, vec![('B', 'A'), ('C', 'B')]);
        assert!(got[1].delete.is_empty() && got[1].moves.is_empty());
        for bad in [&br#"{"tracks": []}"#[..], br#"{"tracks": [{"id": "x"}]}"#, br#"{"tracks": [{"id": "1", "move": [["B"]]}]}"#,
            br#"{"tracks": [{"id": "1", "delete": ["Z"]}]}"#] {
            assert!(parse_arrangements(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn an_import_needs_an_absolute_path() {
        assert!(parse_import(br#"{"path": "C:/Music/x.flac"}"#).is_ok());
        for bad in [&br#"{"path": "x.flac"}"#[..], br#"{"path": ""}"#, br#"{}"#, b"nope"] {
            assert!(parse_import(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn a_new_playlist_needs_a_clean_name_and_tracks_that_are_content_ids() {
        let ok = parse_new_playlist(br#"{"name": "  Friday warm-up ", "tracks": ["12", 34]}"#).unwrap();
        assert_eq!(ok, ("Friday warm-up".to_owned(), vec!["12".to_owned(), "34".to_owned()]));
        for bad in [
            &br#"{"name": "", "tracks": ["1"]}"#[..],
            br#"{"name": "   ", "tracks": ["1"]}"#,
            br#"{"tracks": ["1"]}"#,
            br#"{"name": "a\u0007b", "tracks": ["1"]}"#,
            br#"{"name": "x", "tracks": []}"#,
            br#"{"name": "x", "tracks": ["1", "abc"]}"#,
            br#"{"name": "x"}"#,
            b"not json",
        ] {
            assert!(parse_new_playlist(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        let long = format!(r#"{{"name": "{}", "tracks": ["1"]}}"#, "x".repeat(201));
        assert!(parse_new_playlist(long.as_bytes()).is_err(), "over 200 characters");
        let many = format!(r#"{{"name": "x", "tracks": [{}]}}"#, vec!["1"; 501].join(","));
        assert!(parse_new_playlist(many.as_bytes()).is_err(), "over 500 tracks");
    }

    #[test]
    fn hot_cues_are_letters_a_to_h_at_a_position_in_milliseconds() {
        let ok = parse_hot_cues(br#"{"cues": [{"slot": "b", "ms": 88275}, {"slot": "F", "ms": 0}]}"#).unwrap();
        assert_eq!(ok, vec![('B', 88275), ('F', 0)], "a letter is read in either case");
        for bad in [
            &br#"{"cues": []}"#[..],
            br#"{"cues": [{"slot": "I", "ms": 1}]}"#,
            br#"{"cues": [{"slot": "BC", "ms": 1}]}"#,
            br#"{"cues": [{"slot": "B"}]}"#,
            br#"{"cues": [{"slot": "B", "ms": -5}]}"#,
            br#"{"cues": [{"slot": "B", "ms": 1.5}]}"#,
            br#"{"cues": [{"slot": "B", "ms": 86400001}]}"#,
            br#"{"cues": [{"slot": 2, "ms": 1}]}"#,
            br#"{"slots": []}"#,
            b"[]",
        ] {
            assert!(parse_hot_cues(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        let many = format!(r#"{{"cues": [{}]}}"#, vec![r#"{"slot": "A", "ms": 1}"#; 17].join(","));
        assert!(parse_hot_cues(many.as_bytes()).is_err(), "over 16 cues");
    }

    fn parse(text: &str) -> Result<Request, (u16, &'static str)> {
        read_request(&mut text.as_bytes())
    }

    #[test]
    fn a_request_is_read_with_its_headers_and_body() {
        let r = parse("POST /v1/playlists/7/tracks?x=1 HTTP/1.1\r\nHost: 127.0.0.1:9\r\nContent-Length: 5\r\nAuthorization: Bearer t\r\n\r\nhello")
            .unwrap();
        assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/v1/playlists/7/tracks"));
        assert_eq!(r.body, b"hello");
        assert_eq!(r.header("host"), Some("127.0.0.1:9"));
        assert!(authorized(&r, "t"));
        assert!(!authorized(&r, "u"));
        assert!(!authorized(&r, "tt"));
    }

    #[test]
    fn a_missing_or_malformed_token_is_not_authorized() {
        let r = parse("GET /v1/status HTTP/1.1\r\nHost: localhost:9\r\n\r\n").unwrap();
        assert!(!authorized(&r, "t"));
        let r = parse("GET /v1/status HTTP/1.1\r\nAuthorization: Basic t\r\n\r\n").unwrap();
        assert!(!authorized(&r, "t"));
    }

    #[test]
    fn only_this_listeners_own_host_names_are_accepted() {
        let ok = |h: &str| host_ok(&parse(&format!("GET / HTTP/1.1\r\nHost: {h}\r\n\r\n")).unwrap(), 8721);
        assert!(ok("127.0.0.1:8721") && ok("localhost:8721"));
        assert!(!ok("evil.example:8721") && !ok("127.0.0.1:1") && !ok("rebind.attacker.com"));
        assert!(!host_ok(&parse("GET / HTTP/1.1\r\n\r\n").unwrap(), 8721));
    }

    #[test]
    fn oversized_and_truncated_requests_are_refused() {
        assert_eq!(parse("POST / HTTP/1.1\r\nContent-Length: 9999999\r\n\r\n").map(|_| ()).unwrap_err().0, 413);
        assert_eq!(parse("POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nshort").map(|_| ()).unwrap_err().0, 400);
        assert_eq!(parse("GET /").map(|_| ()).unwrap_err().0, 400);
        let big = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD + 10));
        assert_eq!(parse(&big).map(|_| ()).unwrap_err().0, 431);
    }

    #[test]
    fn a_content_id_is_digits_given_as_a_string_or_a_number() {
        assert_eq!(content_id(&json!("123")), Some("123".into()));
        assert_eq!(content_id(&json!(456)), Some("456".into()));
        for bad in [json!(""), json!("12a"), json!(-1), json!(1.5), json!(null), json!(["1"])] {
            assert_eq!(content_id(&bad), None, "{bad}");
        }
    }
}
