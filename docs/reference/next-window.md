# The Next window

> Fork only (`tonnkatonno/rbxport`). Not proposed for the core project.

What to play after a playlist's last track. **View › Next Window** (Shift+Ctrl/Cmd+N) opens it.

Pick a plain playlist. The left pane lists it in order; the right pane lists the tracks of the library that fit its last
track, best first:

- **Transition**: tempo within 6 %, energy within 2, any harmonic Camelot move (same key, one step, the relative
  major or minor, a +2 energy boost, a fifth away, a three-step mood shift).
- **Double drop**: two tracks played at once, so the filter is stricter: tempo within 3 %, energy within 1, and the key
  at most one step away on the wheel (same, one number up or down, or the relative key).

Click a track in the left pane to see what follows *it* instead of the last one; **Follow the last track** goes back.
The playlist's own tracks are never suggested.

| Control | Does |
| --- | --- |
| ▶ / ■ | Previews the suggestion from the start (the browser's preview player, nothing is loaded onto a deck) |
| Add | Adds the track at the end of the playlist, through the same edit as a drag in the browser (so a player on LINK sees it when it browses; like that drag it is not on the Undo stack) |
| score | 0 to 100: 100, less 8 per key step, 200 per unit of tempo difference and 5 per energy level, plus the transition's bonus |
| ★ N× | The track came right after the seed N times in your own plain playlists and history sessions. It adds 14 (once) to 30 to the ranking, so what you have played after it before comes first, ahead of the many that merely fit (the shown score stops at 100, the ranking does not) |

The window asks again on `library:changed`, so tracks added here (or by another program through the
[local API](local-api.md)) show in the list at once and the suggestions move on to the new last track.

**Library Protection** applies as it does in the main window: with it on (the default) *Add* is refused with the same
message; rekordbox running refuses it too.

## Build: a whole chain from one start

The **Build** view makes a run of tracks from the same start (the playlist's last track, or the one clicked in the left
pane). The **pattern** has one letter a track, `T` for a transition and `D` for a double drop, and repeats to the number of
tracks asked for: `DT` with 8 tracks is double, transition, double, transition, and so on. Each track has to fit the one
before it as its letter asks, with the Suggestions rules above.

- It is a beam search (10 chains kept, 6 tries after each) that maximises the ranks added up, so a pair you have played
  before leads when what follows it fits too. No track is used twice, none already in the playlist, and none strays from
  the start's tempo by more than 8 % however many steps of 6 % it took to get there.
- It stops short, and says so, when nothing fits after the last track.
- **Shuffle** asks again with another seed for the order among equally good tracks (a library has hundreds at the same key,
  tempo and energy, and without it they would come out in title order).
- Nothing is written until you choose: **Create playlist** makes a new top-level playlist, the start track first, as one
  edit; **Add all to the playlist** puts the chain at the end of the playlist picked. Library Protection refuses both.
  Neither is on the Undo stack (creating a playlist in the browser is not either): Delete Playlist removes a built one.
- A `D` is a property of the *pair* (the track and the one before it). The pair is chosen on key, tempo and energy only:
  it does not yet know where either track's drop is. The searcher's drop detection is what would add that.

## Where the numbers come from

Energy is read from the comment, where Mixed In Key leaves `4A - Energy 7`; a track without a level counts as no
difference. A track with no key or no tempo can neither be the seed nor be suggested.

The ranking is `rbl_index::suggest` (`crates/rbl-index/src/suggest.rs`), a port of the double-drop searcher's Stage 1 gate,
so the two programs agree. It is a few integer compares a row: a whole library takes milliseconds.

## Code

- `crates/rbl-index/src/suggest.rs`: the pure ranking, the chain search and their tests.
- `src-tauri/src/next_window.rs`: the `open_next_window`, `suggest_next`, `build_next` and `create_playlist_with_tracks`
  commands.
- `src/views/next/NextWindow.tsx`: the view; routed from `src/main.tsx` at `#next`.
- `src-tauri/src/menu.rs`: the View menu item.
