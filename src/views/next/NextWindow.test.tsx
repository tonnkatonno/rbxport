/** @vitest-environment jsdom */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { __setBackend } from "@/ipc/client";
import type { Backend, NextItem, NextResult, NextTrack } from "@/ipc/types";
import { PREFERENCES_KEY } from "@/lib/preferences";
import { __resetPreview } from "@/store/usePreview";
import { NextWindow } from "./NextWindow";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean;
}

let host: HTMLDivElement;
let root: Root;

const track = (id: string, title: string, key = "4A", bpm = 174): NextTrack =>
  ({ id, title, artist: `${title} Artist`, key, bpm, energy: 7, lengthSec: 225 });
const item = (t: NextTrack, followed = 0): NextItem =>
  ({ track: t, score: 100, keyDistance: 0, transition: "SAME", bpmDiffPct: 0, energyDiff: 0, followed });

const playlist = [track("10", "First"), track("11", "Last")];
const result = (mode: string): NextResult => ({
  playlistId: "7",
  playlistName: "My set",
  tracks: playlist,
  seed: playlist[1] ?? null,
  items: mode === "double" ? [item(track("21", "Tight"))] : [item(track("20", "Next Song"), 3), item(track("21", "Tight"))],
});

const suggestNext = vi.fn((_playlist: string, _seed: string | null, mode: string) => Promise.resolve(result(mode)));
const addTracksToPlaylist = vi.fn((_playlist: string, _tracks: string[]) => Promise.resolve(1));
let libraryChanged: (() => void) | undefined;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  vi.useFakeTimers();
  localStorage.clear();
  libraryChanged = undefined;
  __setBackend({
    playlistTree: () => Promise.resolve([
      { id: "all", name: "All Tracks", kind: "allTracks", depth: 0 },
      { id: "7", name: "My set", kind: "playlist", depth: 2 },
    ]),
    suggestNext,
    onLibraryChanged: (listener: () => void) => { libraryChanged = listener; return () => { libraryChanged = undefined; }; },
    edits: { addTracksToPlaylist },
    previewPlay: () => Promise.resolve(),
    previewStop: () => Promise.resolve(),
    previewState: () => Promise.resolve({ track: null, playing: false, positionMs: 0, durationMs: 0 }),
    closeWindow: () => Promise.resolve(),
  } as unknown as Backend);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  __setBackend(null);
  __resetPreview();
  vi.useRealTimers();
  vi.clearAllMocks();
});

async function flush() {
  await act(async () => { await vi.advanceTimersByTimeAsync(0); });
}

function pickPlaylist(id: string) {
  const select = host.querySelector("select")!;
  act(() => {
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(select, id);
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

const button = (text: string) => [...host.querySelectorAll("button")].find(b => b.textContent?.trim() === text)!;
const suggestions = () => [...host.querySelectorAll("[role=list]")[1]!.querySelectorAll("[role=listitem]")].map(row => row.textContent);

async function open(protectLibrary: boolean) {
  localStorage.setItem(PREFERENCES_KEY, JSON.stringify({ advanced: { protectLibrary } }));
  act(() => root.render(<NextWindow />));
  await flush();
  pickPlaylist("7");
  await flush();
}

it("lists the playlist and what fits after its last track, with what was played after it before", async () => {
  await open(true);
  expect(suggestNext).toHaveBeenLastCalledWith("7", null, "transition", 80);
  expect(host.textContent).toContain("After “Last”");
  expect(host.querySelectorAll("[role=list]")[0]!.querySelectorAll("[role=listitem]")).toHaveLength(2);
  const rows = suggestions();
  expect(rows).toHaveLength(2);
  expect(rows[0]).toContain("Next Song");
  expect(rows[0]).toContain("★ 3×");
  expect(rows[1]).not.toContain("★");
});

it("asks for the stricter double drop when that mode is picked, and follows a clicked track", async () => {
  await open(true);
  act(() => button("Double drop").click());
  await flush();
  expect(suggestNext).toHaveBeenLastCalledWith("7", null, "double", 80);
  expect(suggestions()).toHaveLength(1);

  act(() => (host.querySelectorAll("[role=list]")[0]!.querySelector("[role=listitem]") as HTMLElement).click());
  await flush();
  expect(suggestNext).toHaveBeenLastCalledWith("7", "10", "double", 80);
});

it("refuses to add while Library Protection is on", async () => {
  await open(true);
  act(() => button("Add").click());
  await flush();
  expect(addTracksToPlaylist).not.toHaveBeenCalled();
  expect(host.querySelector("[role=status]")?.textContent).toContain("Library Protection");
});

it("adds a suggestion and keeps saying so when the library change makes it ask again", async () => {
  await open(false);
  act(() => button("Add").click());
  await flush();
  expect(addTracksToPlaylist).toHaveBeenCalledWith("7", ["20"]);
  expect(host.querySelector("[role=status]")?.textContent).toBe("Added “Next Song”.");

  const asked = suggestNext.mock.calls.length;
  act(() => libraryChanged?.());
  await flush();
  expect(suggestNext.mock.calls.length).toBeGreaterThan(asked);
  expect(host.querySelector("[role=status]")?.textContent).toBe("Added “Next Song”.");
});
