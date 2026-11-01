/**
 * The Next window: what to play after a playlist's last track, and a chain built from it.
 *
 * Fork only. Pick a plain playlist and the window lists it beside the tracks of the library that fit its last one (or the
 * track clicked in the list): a plain transition, or a double drop, which is stricter about tempo, energy and key because
 * the two tracks play at once. A preview plays a suggestion without loading it, and "Add" puts it at the end of the
 * playlist through the ordinary edit, so the window asks again when `library:changed` arrives and the list grows in step
 * with the library, in rbxport or from another program.
 *
 * "Build" makes a whole run of tracks from the same start: a pattern of `T` (transition) and `D` (double drop), one letter
 * a track, repeating to the length asked for. The chain is only shown until "Create playlist" makes a playlist of it (the
 * start track first), or "Add all" puts it at the end of the playlist picked.
 *
 * "Grids" (only when rbxport was started with Grid A/B, see `GridsView.tsx`) chooses which of two analyses of a track the
 * library uses, by ear, with the deck's metronome.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { getBackend } from "@/ipc/client";
import type { NextBuild, NextMode, NextResult, NextTrack, TreeNode } from "@/ipc/types";
import { useTranslation } from "@/i18n";
import { refusal } from "@/lib/menu";
import { startWindowDrag } from "@/lib/windowDrag";
import { useShowWindowWhenReady } from "@/lib/windowReady";
import { usePreferencesStore } from "@/store/usePreferences";
import { onPreviewError, startPreview, stopPreview, usePreview } from "@/store/usePreview";
import { GridsView } from "./GridsView";
import styles from "./NextWindow.module.css";

const STORAGE_KEY = "rbxport.next";
/** How many suggestions one request asks for. */
const LIMIT = 80;
/** The most tracks one build makes, as the backend allows. */
const MAX_LENGTH = 60;
const DEFAULT_PATTERN = "DT";
const DEFAULT_LENGTH = 8;

type View = "suggest" | "build" | "grids";

function message(error: unknown): string {
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string") return error.message;
  return String(error);
}

interface Remembered {
  playlist: string;
  mode: NextMode;
  view: View;
  pattern: string;
  length: number;
}

/** What the window remembers between runs. */
function remembered(): Remembered {
  const plain: Remembered = { playlist: "", mode: "transition", view: "suggest", pattern: DEFAULT_PATTERN, length: DEFAULT_LENGTH };
  try {
    const stored = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "{}") as Partial<Record<keyof Remembered, unknown>>;
    return {
      playlist: typeof stored.playlist === "string" ? stored.playlist : plain.playlist,
      mode: stored.mode === "double" ? "double" : "transition",
      view: stored.view === "build" || stored.view === "grids" ? stored.view : "suggest",
      pattern: typeof stored.pattern === "string" && stored.pattern.length > 0 ? stored.pattern : plain.pattern,
      length: typeof stored.length === "number" && stored.length >= 1 ? Math.min(MAX_LENGTH, Math.floor(stored.length)) : plain.length,
    };
  } catch {
    return plain;
  }
}

function remember(state: Remembered): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch { /* Private browsing or a full quota: the choice is just not kept. */ }
}

function clock(seconds: number): string {
  if (seconds <= 0) return "";
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

function tempo(track: NextTrack): string {
  return track.bpm > 0 ? track.bpm.toFixed(2) : "";
}

/** The letters a pattern may hold, with the separators the backend ignores. */
const PATTERN_OK = /^[TtDd\s,\-/>+·]*[TtDd][TtDd\s,\-/>+·]*$/;

export function NextWindow() {
  const t = useTranslation();
  const initial = useMemo(remembered, []);
  const [lists, setLists] = useState<TreeNode[] | null>(null);
  const [playlist, setPlaylist] = useState(initial.playlist);
  const [mode, setMode] = useState<NextMode>(initial.mode);
  const [view, setView] = useState<View>(initial.view);
  const [pattern, setPattern] = useState(initial.pattern);
  const [length, setLength] = useState(initial.length);
  // The track the suggestions follow; null is the playlist's last one, and follows it as tracks are added.
  const [seed, setSeed] = useState<string | null>(null);
  const [result, setResult] = useState<NextResult | null>(null);
  const [built, setBuilt] = useState<NextBuild | null>(null);
  // Counts the "Shuffle" presses: each one picks differently among equally good tracks.
  const [variety, setVariety] = useState(0);
  const [nameDraft, setNameDraft] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; failed: boolean } | null>(null);
  const [loaded, setLoaded] = useState(false);
  // Grid A/B (a store of two analyses per track) is off unless rbxport was started with it.
  const [gridAb, setGridAb] = useState(false);
  const request = useRef(0);
  useShowWindowWhenReady(loaded);
  // Library Protection is the same switch the main window obeys: a preference both windows read, the backend cannot see it.
  const protectLibrary = usePreferencesStore().preferences.advanced.protectLibrary;

  const fail = useCallback((error: unknown) => setNotice({ text: message(error), failed: true }), []);

  // The plain playlists, in tree order, for the picker.
  const loadLists = useCallback(async () => {
    try {
      const backend = await getBackend();
      const tree = await backend.playlistTree();
      setLists(tree.filter((node) => node.kind === "playlist"));
    } catch (error) {
      fail(error);
    } finally {
      setLoaded(true);
    }
  }, [fail]);

  const refresh = useCallback(async () => {
    if (!playlist) { setResult(null); return; }
    const mine = ++request.current;
    try {
      const backend = await getBackend();
      const next = await backend.suggestNext(playlist, seed, mode, LIMIT);
      // An answer clears an earlier failure, but not "Added …": the library change that edit announced asks again at once.
      if (mine === request.current) { setResult(next); setNotice((shown) => (shown?.failed ? null : shown)); }
    } catch (error) {
      if (mine === request.current) { setResult(null); fail(error); }
    }
  }, [playlist, seed, mode, fail]);

  useEffect(() => { void loadLists(); }, [loadLists]);
  useEffect(() => {
    void getBackend().then((backend) => backend.gridAbStatus()).then((status) => {
      setGridAb(status.enabled);
      if (!status.enabled) setView((now) => (now === "grids" ? "suggest" : now));
    }).catch(() => setGridAb(false));
  }, []);
  const onGridNotice = useCallback((text: string, failed: boolean) => setNotice({ text, failed }), []);
  useEffect(() => { void refresh(); }, [refresh]);
  useEffect(() => { remember({ playlist, mode, view, pattern, length }); }, [playlist, mode, view, pattern, length]);
  // A chain belongs to the start it was built from.
  useEffect(() => { setBuilt(null); setNameDraft(null); }, [playlist, seed]);

  // The library changed (an edit here, or another program's): the playlist may have grown or been renamed.
  useEffect(() => {
    let off: (() => void) | undefined;
    let live = true;
    void getBackend().then((backend) => {
      const unsubscribe = backend.onLibraryChanged(() => { void loadLists(); void refresh(); });
      if (live) off = unsubscribe; else unsubscribe();
    });
    return () => { live = false; off?.(); };
  }, [loadLists, refresh]);

  // A picked seed that left the playlist falls back to the last track.
  useEffect(() => {
    if (seed && result && !result.tracks.some((track) => track.id === seed)) setSeed(null);
  }, [seed, result]);

  // The preview ends by itself, and the button follows: the store the browser's rows use asks the backend while it plays.
  const previewing = usePreview();
  const playing = previewing.playing ? previewing.trackId : null;
  useEffect(() => onPreviewError((text) => setNotice({ text, failed: true })), []);
  useEffect(() => () => { void stopPreview(); }, []);

  const preview = (track: NextTrack) => {
    if (playing === track.id) void stopPreview();
    else void startPreview(track.id, 0, track.lengthSec * 1000);
  };

  const add = (track: NextTrack) => {
    if (protectLibrary) {
      setNotice({ text: t(refusal(true)), failed: true });
      return;
    }
    void getBackend().then(async (backend) => {
      await backend.edits.addTracksToPlaylist(playlist, [track.id]);
      // The playlist grew: the suggestions follow its new last track.
      setSeed(null);
      setNotice({ text: t("Added “{title}”.", { title: track.title }), failed: false });
    }).catch(fail);
  };

  const patternValid = PATTERN_OK.test(pattern);
  const build = (nextVariety: number) => {
    setVariety(nextVariety);
    void getBackend().then(async (backend) => {
      const chain = await backend.buildNext(playlist, seed, pattern, length, nextVariety);
      setBuilt(chain);
      setNameDraft(null);
      setNotice(chain.steps.length === 0 ? { text: t("Nothing in the library fits the start track."), failed: true } : null);
    }).catch((error: unknown) => { setBuilt(null); fail(error); });
  };

  const chainIds = built ? built.steps.map((step) => step.item.track.id) : [];
  const defaultName = built ? `${built.seed.title} · ${pattern.replace(/[^TtDd]/g, "").toUpperCase()}` : "";
  const chainName = (nameDraft ?? defaultName).trim();

  const createChain = () => {
    if (!built) return;
    if (protectLibrary) {
      setNotice({ text: t(refusal(true)), failed: true });
      return;
    }
    void getBackend().then(async (backend) => {
      await backend.edits.createPlaylistWithTracks(chainName, [built.seed.id, ...chainIds]);
      setNotice({ text: t("Created “{name}” with {count} tracks.", { name: chainName, count: chainIds.length + 1 }), failed: false });
    }).catch(fail);
  };

  const addChain = () => {
    if (!built) return;
    if (protectLibrary) {
      setNotice({ text: t(refusal(true)), failed: true });
      return;
    }
    void getBackend().then(async (backend) => {
      await backend.edits.addTracksToPlaylist(playlist, chainIds);
      // The playlist grew: the chain was built from where it used to end.
      setSeed(null);
      setBuilt(null);
      setNotice({ text: t("Added {count} tracks to “{name}”.", { count: chainIds.length, name: result?.playlistName ?? "" }), failed: false });
    }).catch(fail);
  };

  useEffect(() => {
    if (!notice || notice.failed) return;
    const timer = setTimeout(() => setNotice(null), 3000);
    return () => clearTimeout(timer);
  }, [notice]);

  const seedId = result?.seed?.id ?? null;
  const lastId = result?.tracks.at(-1)?.id ?? null;
  const close = () => { void getBackend().then((backend) => backend.closeWindow()); };

  /** The play button every track row has. */
  const playButton = (track: NextTrack) => (
    <button type="button" className={styles.play} data-on={playing === track.id || undefined}
      aria-label={playing === track.id ? t("Stop the preview") : t("Preview")}
      onClick={() => preview(track)}>{playing === track.id ? "■" : "▶"}</button>
  );

  return (
    <section className={styles.window} role="dialog" aria-label={t("Next")}>
      <header className={styles.title} onMouseDown={startWindowDrag}>{t("Next")}</header>
      <div className={styles.controls}>
        <label className={styles.picker}>
          <span>{t("Playlist")}</span>
          <select value={playlist} onChange={(event) => { setPlaylist(event.target.value); setSeed(null); }}>
            <option value="">{view === "grids" ? t("All tracks") : t("Pick a playlist")}</option>
            {(lists ?? []).map((node) => (
              <option key={node.id} value={node.id}>{"  ".repeat(Math.max(0, node.depth - 2))}{node.name}</option>
            ))}
          </select>
        </label>
        <div className={styles.modes} role="radiogroup" aria-label={t("View")}>
          {(gridAb ? ["suggest", "build", "grids"] as const : ["suggest", "build"] as const).map((choice) => (
            <button key={choice} type="button" role="radio" aria-checked={view === choice} data-on={view === choice || undefined}
              onClick={() => setView(choice)}>{choice === "suggest" ? t("Suggestions") : choice === "build" ? t("Build") : t("Grids")}</button>
          ))}
        </div>
        {view === "suggest" ? (
          <div className={styles.modes} role="radiogroup" aria-label={t("Mode")}>
            {(["transition", "double"] as const).map((choice) => (
              <button key={choice} type="button" role="radio" aria-checked={mode === choice} data-on={mode === choice || undefined}
                onClick={() => setMode(choice)}>{choice === "transition" ? t("Transition") : t("Double drop")}</button>
            ))}
          </div>
        ) : null}
        {view === "grids" ? null : <button type="button" disabled={seed === null} onClick={() => setSeed(null)}>{t("Follow the last track")}</button>}
        <span className={styles.count}>
          {view === "suggest" && result ? t("{count} suggestions", { count: result.items.length }) : ""}
        </span>
        <button type="button" onClick={close}>{t("Close")}</button>
      </div>
      {view === "build" ? (
        <div className={styles.buildBar}>
          <label className={styles.patternBox}>
            <span>{t("Pattern")}</span>
            <input type="text" value={pattern} spellCheck={false} aria-invalid={!patternValid || undefined}
              title={t("T is a transition, D is a double drop, one letter a track; it repeats to the length, for example DT or TTD")}
              onChange={(event) => setPattern(event.target.value)} />
          </label>
          <label className={styles.countBox}>
            <span>{t("Tracks")}</span>
            <input type="number" min={1} max={MAX_LENGTH} value={length}
              onChange={(event) => setLength(Math.min(MAX_LENGTH, Math.max(1, Math.floor(Number(event.target.value)) || 1)))} />
          </label>
          <button type="button" disabled={!playlist || !patternValid} onClick={() => build(0)}>{t("Build chain")}</button>
          <button type="button" disabled={!built} title={t("Another chain: equally good tracks are picked differently")}
            onClick={() => build(variety + 1)}>{t("Shuffle")}</button>
          <span className={styles.count}>
            {built ? t("{count} of {requested} tracks", { count: built.steps.length, requested: built.requested }) : ""}
          </span>
        </div>
      ) : null}

      {view === "grids" ? <GridsView playlist={playlist} protectLibrary={protectLibrary} onNotice={onGridNotice} /> : (
      <div className={styles.panes}>
        <div className={styles.pane} aria-label={t("The playlist")}>
          <h2>{result ? result.playlistName : t("The playlist")}</h2>
          <div className={styles.rows} role="list">
            {result && result.tracks.length === 0 ? <p className={styles.note}>{t("The playlist is empty: add a track first.")}</p> : null}
            {(result?.tracks ?? []).map((track, index) => (
              <button key={`${track.id}-${index}`} type="button" role="listitem" className={styles.track}
                data-seed={track.id === seedId || undefined} data-last={track.id === lastId || undefined}
                title={t("Suggest what follows this track")}
                onClick={() => setSeed(track.id === lastId ? null : track.id)}>
                <span className={styles.num}>{index + 1}</span>
                <span className={styles.name}><b>{track.title}</b><i>{track.artist}</i></span>
                <span className={styles.key}>{track.key}</span>
                <span className={styles.bpm}>{tempo(track)}</span>
              </button>
            ))}
          </div>
        </div>

        {view === "suggest" ? (
          <div className={styles.pane} aria-label={t("Suggestions")}>
            <h2>{result?.seed ? t("After “{title}”", { title: result.seed.title }) : t("Suggestions")}</h2>
            <div className={styles.rows} role="list">
              {!playlist ? <p className={styles.note}>{t("Pick a playlist to see what fits after its last track.")}</p> : null}
              {playlist && result && !result.seed ? <p className={styles.note}>{t("Nothing to follow yet.")}</p> : null}
              {result?.seed && result.items.length === 0
                ? <p className={styles.note}>{result.seed.key && result.seed.bpm > 0
                  ? t("Nothing in the library fits.") : t("The track has no key or tempo to match on.")}</p> : null}
              {(result?.items ?? []).map((item) => (
                <div key={item.track.id} role="listitem" className={styles.suggestion}>
                  {playButton(item.track)}
                  <span className={styles.score} title={t("How well it fits, out of 100")}>
                    <i style={{ width: `${Math.round(item.score)}%` }} />
                    <b>{Math.round(item.score)}</b>
                  </span>
                  <span className={styles.name}><b>{item.track.title}</b><i>{item.track.artist}</i></span>
                  <span className={styles.key}>{item.track.key}</span>
                  <span className={styles.bpm}>{tempo(item.track)}</span>
                  <span className={styles.delta} title={t("Tempo difference from the seed")}>
                    {item.bpmDiffPct === 0 ? "±0%" : `${item.bpmDiffPct.toFixed(1)}%`}
                  </span>
                  <span className={styles.energy}>{item.track.energy > 0 ? `E${item.track.energy}` : ""}</span>
                  <span className={styles.length}>{clock(item.track.lengthSec)}</span>
                  <span className={styles.played} title={t("Played after the seed in your playlists or histories")}>
                    {item.followed > 0 ? `★ ${item.followed}×` : ""}
                  </span>
                  <span className={styles.kind}>{item.transition.replace("_", " ")}</span>
                  <button type="button" className={styles.add} onClick={() => add(item.track)}>{t("Add")}</button>
                </div>
              ))}
            </div>
          </div>
        ) : (
          <div className={styles.pane} aria-label={t("Chain")}>
            <h2>{built ? t("Chain after “{title}”", { title: built.seed.title }) : t("Chain")}</h2>
            <div className={styles.rows} role="list">
              {!playlist ? <p className={styles.note}>{t("Pick a playlist to start from its last track.")}</p> : null}
              {playlist && !built
                ? <p className={styles.note}>{t("Choose a pattern and the number of tracks, then Build. Click a track of the playlist to start from it.")}</p> : null}
              {built && built.steps.length < built.requested && built.steps.length > 0
                ? <p className={styles.note}>{t("Nothing fitted after track {count}, so the chain stops there.", { count: built.steps.length })}</p> : null}
              {(built?.steps ?? []).map((step, index) => (
                <div key={step.item.track.id} role="listitem" className={styles.suggestion}>
                  {playButton(step.item.track)}
                  <span className={styles.num}>{index + 1}</span>
                  <span className={styles.stepMode} data-mode={step.mode}
                    title={step.mode === "D" ? t("Double drop with the track before") : t("Transition from the track before")}>{step.mode}</span>
                  <span className={styles.name}><b>{step.item.track.title}</b><i>{step.item.track.artist}</i></span>
                  <span className={styles.key}>{step.item.track.key}</span>
                  <span className={styles.bpm}>{tempo(step.item.track)}</span>
                  <span className={styles.energy}>{step.item.track.energy > 0 ? `E${step.item.track.energy}` : ""}</span>
                  <span className={styles.length}>{clock(step.item.track.lengthSec)}</span>
                  <span className={styles.played} title={t("Played after the track before in your playlists or histories")}>
                    {step.item.followed > 0 ? `★ ${step.item.followed}×` : ""}
                  </span>
                  <span className={styles.kind}>{step.item.transition.replace("_", " ")}</span>
                </div>
              ))}
            </div>
            {built && built.steps.length > 0 ? (
              <div className={styles.chainFooter}>
                <input type="text" className={styles.nameBox} aria-label={t("Name of the new playlist")} value={nameDraft ?? defaultName}
                  onChange={(event) => setNameDraft(event.target.value)} />
                <button type="button" className={styles.add} disabled={chainName === ""} onClick={createChain}>{t("Create playlist")}</button>
                <button type="button" className={styles.add} onClick={addChain}>{t("Add all to the playlist")}</button>
              </div>
            ) : null}
          </div>
        )}
      </div>
      )}
      {notice ? <p className={notice.failed ? styles.error : styles.status} role="status">{notice.text}</p> : null}
    </section>
  );
}
