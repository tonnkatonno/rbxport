/**
 * The Grids view of the Next window: choose, track by track, which of two analyses the library uses.
 *
 * Fork only. Where rekordbox's analysis and rbxport's differ enough for a metronome to tell them apart, the row has a switch
 * between the two. ▶ puts the track on deck A with the metronome on and plays it from a little way in; pressing the other
 * source while it plays moves the clicks to that grid at once, so the two can be heard one after the other: a grid that is
 * right has its clicks on the kicks. A check that ran outside rbxport (see `docs/reference/grid-ab.md`) suggests a source for
 * the tracks it could tell; "Apply suggestions" takes those, and the rest are for the ear.
 */
import { useCallback, useEffect, useRef, useState } from "react";

import { getBackend } from "@/ipc/client";
import type { GridAbApplied, GridAbRow, GridSource } from "@/ipc/types";
import { useTranslation } from "@/i18n";
import { refusal } from "@/lib/menu";
import { usePlayback } from "@/store/usePlayback";
import styles from "./NextWindow.module.css";

/** How far into a track the audition starts, in seconds: past an intro with no drums, as far as the track is long. */
const AUDITION_FROM = 45;

function message(error: unknown): string {
  if (error && typeof error === "object" && "message" in error && typeof error.message === "string") return error.message;
  return String(error);
}

/** Plays one track on deck A with the metronome on, from `AUDITION_FROM`, and stops and clicks no more when it unmounts. */
function Audition({ trackId }: { trackId: string }) {
  const playback = usePlayback(trackId, "a", false);
  const started = useRef<string | null>(null);
  const { duration, seek, toggle, playing } = playback;
  useEffect(() => {
    void getBackend().then((backend) => backend.deckMetronome("a", true)).catch(() => {});
    return () => {
      void getBackend().then(async (backend) => {
        await backend.deckPause("a");
        await backend.deckMetronome("a", false);
      }).catch(() => {});
    };
  }, []);
  useEffect(() => {
    if (duration <= 0 || started.current === trackId) return;
    started.current = trackId;
    seek(Math.min(AUDITION_FROM, duration * 0.3));
    if (!playing) toggle();
  }, [duration, trackId, seek, toggle, playing]);
  return null;
}

function bpm(value: number): string {
  return value > 0 ? value.toFixed(2) : "–";
}

function ms(value: number): string {
  return `${value > 0 ? "+" : ""}${value.toFixed(0)} ms`;
}

export function GridsView({ playlist, protectLibrary, onNotice }: {
  playlist: string;
  protectLibrary: boolean;
  onNotice: (text: string, failed: boolean) => void;
}) {
  const t = useTranslation();
  const [rows, setRows] = useState<GridAbRow[] | null>(null);
  // Only where a metronome can tell the two apart, or every track the store holds.
  const [all, setAll] = useState(false);
  const [earOnly, setEarOnly] = useState(false);
  const [auditioning, setAuditioning] = useState<string | null>(null);
  const request = useRef(0);

  const refresh = useCallback(async () => {
    const mine = ++request.current;
    try {
      const backend = await getBackend();
      const list = await backend.gridAbList(playlist === "" ? null : playlist, all);
      if (mine === request.current) setRows(list);
    } catch (error) {
      if (mine === request.current) { setRows([]); onNotice(message(error), true); }
    }
  }, [playlist, all, onNotice]);

  useEffect(() => { void refresh(); }, [refresh]);

  const shown = (rows ?? []).filter((row) => !earOnly || row.suggestion === "ear" || row.suggestion === null);

  const choose = (row: GridAbRow, source: GridSource) => {
    if (row.active === source) return;
    if (protectLibrary) { onNotice(t(refusal(true)), true); return; }
    void getBackend().then(async (backend) => {
      const updated = await backend.gridAbUse(row.track.id, source);
      setRows((held) => (held ?? []).map((r) => (r.track.id === updated.track.id ? updated : r)));
    }).catch((error: unknown) => onNotice(message(error), true));
  };

  const apply = () => {
    if (protectLibrary) { onNotice(t(refusal(true)), true); return; }
    void getBackend().then(async (backend) => {
      const done: GridAbApplied = await backend.gridAbApply(playlist === "" ? null : playlist);
      onNotice(done.failed.length > 0
        ? t("Applied {applied}, {failed} failed: {first}", { applied: done.applied, failed: done.failed.length, first: done.failed[0] ?? "" })
        : t("Applied {applied} suggestions, left {skipped} for the ear.", { applied: done.applied, skipped: done.skipped }), done.failed.length > 0);
      await refresh();
    }).catch((error: unknown) => onNotice(message(error), true));
  };

  const suggested = (rows ?? []).filter((r) => (r.suggestion === "rbxport" || r.suggestion === "rekordbox") && r.suggestion !== r.active).length;

  return (
    <>
      <div className={styles.buildBar}>
        <label className={styles.checkBox}>
          <input type="checkbox" checked={all} onChange={(event) => setAll(event.target.checked)} />
          <span>{t("Include grids a metronome cannot tell apart")}</span>
        </label>
        <label className={styles.checkBox}>
          <input type="checkbox" checked={earOnly} onChange={(event) => setEarOnly(event.target.checked)} />
          <span>{t("Only what needs an ear")}</span>
        </label>
        <button type="button" disabled={suggested === 0} onClick={apply}
          title={t("Use the source the outside check was sure of, for the tracks listed")}>
          {t("Apply suggestions")}{suggested > 0 ? ` (${suggested})` : ""}
        </button>
        <span className={styles.count}>{rows ? t("{count} tracks", { count: shown.length }) : ""}</span>
      </div>
      <div className={styles.panes} data-single>
        <div className={styles.pane} aria-label={t("Grids")}>
          <h2>{playlist === "" ? t("All tracks with two analyses") : t("Tracks of the playlist with two analyses")}</h2>
          <div className={styles.rows} role="list">
            {rows && shown.length === 0 ? <p className={styles.note}>{t("Nothing to choose here.")}</p> : null}
            {shown.map((row) => (
              <div key={row.track.id} role="listitem" className={styles.suggestion} data-playing={auditioning === row.track.id || undefined}>
                <button type="button" className={styles.play} data-on={auditioning === row.track.id || undefined}
                  aria-label={auditioning === row.track.id ? t("Stop the preview") : t("Listen with the metronome")}
                  title={t("Plays on deck A with the metronome; press the other source while it plays to hear the difference")}
                  onClick={() => setAuditioning((now) => (now === row.track.id ? null : row.track.id))}>
                  {auditioning === row.track.id ? "■" : "▶"}
                </button>
                <span className={styles.name}><b>{row.track.title}</b><i>{row.track.artist}</i></span>
                <span className={styles.key}>{row.track.key}</span>
                <span className={styles.tempoPair} title={t("rekordbox / rbxport")}>
                  {bpm(row.rekordboxBpm)} / {row.rbxportEmpty ? t("no grid") : bpm(row.rbxportBpm)}
                </span>
                <span className={styles.phase} title={t("Where rbxport's grid lines are against rekordbox's")}>
                  {row.rbxportEmpty ? "" : ms(row.phaseMs)}
                </span>
                <span className={styles.verdict} data-verdict={row.suggestion ?? "none"}
                  title={t("What the outside check made of the pair")}>
                  {row.suggestion === "ear" ? t("ear") : row.suggestion === "same" ? t("same") : row.suggestion ?? ""}
                </span>
                <span className={styles.switch} role="radiogroup" aria-label={t("Grid in use")}>
                  {(["rekordbox", "rbxport"] as const).map((source) => (
                    <button key={source} type="button" role="radio" aria-checked={row.active === source}
                      data-on={row.active === source || undefined} disabled={source === "rbxport" && row.rbxportEmpty}
                      onClick={() => choose(row, source)}>{source === "rekordbox" ? t("Rekordbox") : t("rbxport")}</button>
                  ))}
                </span>
              </div>
            ))}
          </div>
        </div>
      </div>
      {auditioning ? <Audition trackId={auditioning} key={auditioning} /> : null}
    </>
  );
}
