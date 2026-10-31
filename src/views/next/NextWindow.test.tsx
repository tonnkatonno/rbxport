/** @vitest-environment jsdom */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { __setBackend } from "@/ipc/client";
import type { Backend, NextBuild, NextItem, NextResult, NextTrack } from "@/ipc/types";
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
const createPlaylistWithTracks = vi.fn((_name: string, _tracks: string[]) => Promise.resolve(1));
// A chain of two tracks after the playlist's last one: a double drop, then a transition.
const buildNext = vi.fn((_playlist: string, _seed: string | null, _pattern: string, length: number, _variety: number) =>
  Promise.resolve<NextBuild>({
    seed: playlist[1]!,
    steps: [{ mode: "D", item: item(track("30", "Pair"), 2) }, { mode: "T", item: item(track("31", "Then")) }],
    requested: length,
  }));
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
    buildNext,
    onLibraryChanged: (listener: () => void) => { libraryChanged = listener; return () => { libraryChanged = undefined; }; },
    edits: { addTracksToPlaylist, createPlaylistWithTracks },
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

function typeInto(input: HTMLInputElement, value: string) {
  act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

const field = (label: string) => host.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`) ??
  [...host.querySelectorAll("label")].find(l => l.textContent?.startsWith(label))!.querySelector("input")!;
const chain = () => [...host.querySelectorAll("[role=list]")[1]!.querySelectorAll("[role=listitem]")].map(row => row.textContent);

async function openBuild(protectLibrary: boolean) {
  await open(protectLibrary);
  act(() => button("Build").click());
  await flush();
  // The default pattern and length, built once.
  act(() => button("Build chain").click());
  await flush();
}

it("builds a chain from a pattern and a length, and shuffles for another", async () => {
  await openBuild(true);
  typeInto(field("Pattern"), "dt");
  typeInto(field("Tracks"), "4");
  act(() => button("Build chain").click());
  await flush();
  expect(buildNext).toHaveBeenLastCalledWith("7", null, "dt", 4, 0);
  const rows = chain();
  expect(rows).toHaveLength(2);
  expect(rows[0]).toMatch(/^▶1DPair/);
  expect(rows[0]).toContain("★ 2×");
  expect(rows[1]).toMatch(/^▶2TThen/);
  expect(host.textContent).toContain("2 of 4 tracks");
  expect(host.textContent).toContain("Nothing fitted after track 2");

  act(() => button("Shuffle").click());
  await flush();
  expect(buildNext).toHaveBeenLastCalledWith("7", null, "dt", 4, 1);
});

it("will not build from a pattern that is not made of T and D", async () => {
  await openBuild(true);
  typeInto(field("Pattern"), "DXT");
  expect(button("Build chain").disabled).toBe(true);
  typeInto(field("Pattern"), "T, D");
  expect(button("Build chain").disabled).toBe(false);
});

it("creates a playlist from the chain, the start track first, under a name that can be changed", async () => {
  await openBuild(false);
  expect(field("Name of the new playlist").value).toBe("Last · DT");
  typeInto(field("Name of the new playlist"), "  Friday warm-up ");
  act(() => button("Create playlist").click());
  await flush();
  expect(createPlaylistWithTracks).toHaveBeenCalledWith("Friday warm-up", ["11", "30", "31"]);
  expect(host.querySelector("[role=status]")?.textContent).toBe("Created “Friday warm-up” with 3 tracks.");
});

it("adds the whole chain to the playlist, and builds nothing while Library Protection is on", async () => {
  await openBuild(true);
  act(() => button("Create playlist").click());
  act(() => button("Add all to the playlist").click());
  await flush();
  expect(createPlaylistWithTracks).not.toHaveBeenCalled();
  expect(addTracksToPlaylist).not.toHaveBeenCalled();
  expect(host.querySelector("[role=status]")?.textContent).toContain("Library Protection");

  localStorage.setItem(PREFERENCES_KEY, JSON.stringify({ advanced: { protectLibrary: false } }));
  act(() => { window.dispatchEvent(new StorageEvent("storage", { key: PREFERENCES_KEY })); });
  await flush();
  act(() => button("Add all to the playlist").click());
  await flush();
  expect(addTracksToPlaylist).toHaveBeenCalledWith("7", ["30", "31"]);
  expect(host.querySelector("[role=status]")?.textContent).toBe("Added 2 tracks to “My set”.");
});
