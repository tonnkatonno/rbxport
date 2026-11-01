# Grid A/B: two analyses of a track, chosen by ear

> Fork only (`tonnkatonno/rbxport`). Not proposed for the core project.

A track has one beat grid in the library. When a second analysis exists (rekordbox's own, and rbxport's, made with its tempo
held to the tag), **Grid A/B** keeps both and lets you choose which one the library uses, track by track, while the
metronome is clicking on the deck: a grid that is right has its clicks on the kicks.

It is off unless rbxport is started with `RBXPORT_GRID_AB_DIR` set to the store (below). Nothing in the installed library is
touched by it unless you point the variable at a store made for that library and choose.

## The store

```
<RBXPORT_GRID_AB_DIR>/
  rekordbox/<analysis path>.DAT|EXT|2EX   the analysis rekordbox made (read from its share, never changed)
  rbxport/<analysis path>.DAT|EXT|2EX     the analysis rbxport made
  index.json                              what the list needs: see below
  active.json                             which one the library has now, per track (written by rbxport)
```

`index.json` is written by a script, not by rbxport. One entry per track id:

```json
{ "123": {
    "rekordbox": { "bpm_x100": 17400, "first_ms": 24.0, "beats": 700 },
    "rbxport":   { "bpm_x100": 17400, "first_ms": 171.0, "beats": 700 },
    "beat_ms": 344.8,
    "suggestion": "rbxport", "err_rb": -146.6, "err_rbx": 23.6 } }
```

`suggestion` (optional) is the verdict of a check that ran outside rbxport on the audio: `rbxport` or `rekordbox` where only
that grid agrees with the track's kick drums, `either` where both do, `same` where the two are one grid, `ear` where neither
agrees, which is for you to decide. `err_rb` and `err_rbx` are how far each grid sits from the kicks, in ms.
`pilot/ab_seed.py` in the sandbox builds all of this.

## The Grids view

Next window › **Grids** (when the variable is set). It lists the tracks of the playlist picked (all of them with none) where
the two grids can be told apart by a metronome: lines 15 ms or more apart, or tempos 0.2 BPM or more apart, or rbxport's
grid empty. The ones that need an ear come first.

- **▶** puts the track on deck A with the metronome on and plays it from 45 s in.
- **Rekordbox | rbxport** is the choice. Pressing the other one while the track plays moves the clicks to that grid at once.
- **Apply suggestions** takes the check's clear verdicts for the tracks listed and leaves the rest.

Choosing copies that analysis's three files over the track's own and writes the tempo, through the same crash-safe file
journal and database write the beat grid editor uses, so it is refused as an edit would be (Library Protection, rekordbox
running, a locked grid). Like a grid edit it is not on the Undo stack: choose the other source to go back, which is lossless
because both are kept.

## Code

- `src-tauri/src/grid_ab.rs`: `grid_ab_status`, `grid_ab_list`, `grid_ab_use`, `grid_ab_apply`.
- `src/views/next/GridsView.tsx`: the view (and `Audition`, the deck's metronome).
