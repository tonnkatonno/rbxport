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
