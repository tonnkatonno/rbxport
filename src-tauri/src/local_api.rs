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
mod tests {
    use super::*;

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
