# Local API (searcher fork)

A small HTTP API on `127.0.0.1` for a companion program on the same machine: read the playlists and add tracks to one. It lives in this fork only (`src-tauri/src/local_api.rs`) and is off unless asked for.

| Variable | Meaning |
| --- | --- |
| `RBXPORT_API_PORT` | Turns the API on and names the port (for example `8721`). |
| `RBXPORT_API_WRITE` | `1` lets the API change the library. Without it every write is refused with `403`. |
| `RBXPORT_API_TOKEN_FILE` | Where the token is written (default `<local data dir>/rbxport/api-token`). |

## Safety

- It listens on `127.0.0.1` only, and the `Host` header must name that listener (a DNS-rebinding page gets `403`).
- Every request needs `Authorization: Bearer <token>`. The token is made at start and written to the token file, so a program that can read the user's files can use the API and a web page cannot: a custom header makes a browser ask first, and no CORS answer is ever given.
- Library Protection (Preferences) is a choice only the interface sees, so the API has its own switch (`RBXPORT_API_WRITE`). A write goes through the interface's own edit path (`commands::edit`): the backend's guards still apply (a real install is refused while rekordbox runs or under `RBXPORT_TEST`), the index refreshes, `library:changed` is emitted and a player on LINK sees the change when it browses.

## Routes

`GET /v1/status`, `GET /v1/playlists`, `GET /v1/playlists/{id}` and `POST /v1/playlists/{id}/tracks` with `{"tracks": ["<content id>", ...], "allow_duplicates": false}`. Replies are JSON; an error is `{"error": "..."}`. A track already in the playlist is skipped unless `allow_duplicates` is set; only plain playlists accept tracks (not folders or intelligent playlists).

Also (fork only, for the searcher's chain generator):

- `POST /v1/playlists` with `{"name": "...", "tracks": ["<content id>", ...]}` makes a new top-level playlist holding the tracks in order, as one edit, and answers `{"ok": true, "id": "...", "name": "...", "tracks": n}`. The name is trimmed (1 to 200 characters), 1 to 500 tracks. A name already taken is refused with 409. It is not on the Undo stack; Delete Playlist removes it.
- `POST /v1/playlists` also takes `"allow_duplicates": true` to keep a track listed more than once (the searcher puts one separator row between each double pair).
- `POST /v1/tracks/import` with `{"path": "D:/.../file.flac"}` adds one audio file to the library, the way File › Import does, and answers `{"ok": true, "id": "...", "added": true}`; a file already in the library answers its id with `"added": false`. A folder or a relative path is refused.
- `GET /v1/tracks/{id}/cues` lists a track's cues: `{"kind": raw djmdCue.Kind, "slot": "B" | null, "ms": ..., "loop_out_ms": ...}`; `slot` is the hot cue letter, null for a memory cue.
- `POST /v1/tracks/{id}/hotcues` with `{"cues": [{"slot": "B", "ms": 88275}, ...]}` (slots A to H, 1 to 16 cues, positions up to 24 h) adds hot cues and answers `{"ok": true, "added": [{"slot", "ms", "id"}], "skipped": [{"slot", "reason": "occupied"}]}`. A slot the track already has (or that is asked for twice) is skipped, never replaced. The letters are stored as rekordbox stores them: A, B, C are kinds 1 to 3, and D onward sit one higher (D is 5, E 6, F 7), because kind 4 is the active memory loop.
