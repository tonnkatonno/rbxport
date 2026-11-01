/** @vitest-environment jsdom */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { __setBackend } from "@/ipc/client";
import type { Backend, GridAbRow } from "@/ipc/types";
import { GridsView } from "./GridsView";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean;
}

let host: HTMLDivElement;
let root: Root;

const row = (id: string, title: string, over: Partial<GridAbRow> = {}): GridAbRow => ({
  track: { id, title, artist: `${title} Artist`, key: "4A", bpm: 174, energy: 7, lengthSec: 225 },
  active: "rbxport", rekordboxBpm: 174, rbxportBpm: 174, rbxportEmpty: false, phaseMs: 147, suggestion: null,
  errRb: null, errRbx: null, differs: true, ...over,
});

const rows: GridAbRow[] = [
  row("1", "Needs an ear", { suggestion: "ear" }),
  row("2", "Clear rbxport", { suggestion: "rbxport", active: "rekordbox" }),
  row("3", "Clear rekordbox", { suggestion: "rekordbox" }),
  row("4", "No verdict", { suggestion: null, phaseMs: -60 }),
  row("5", "Empty grid", { suggestion: "rekordbox", rbxportEmpty: true, rbxportBpm: 0 }),
];

const gridAbList = vi.fn((_playlist: string | null, _all: boolean) => Promise.resolve(rows));
const gridAbUse = vi.fn((track: string, source: "rekordbox" | "rbxport") =>
  Promise.resolve({ ...rows.find((r) => r.track.id === track)!, active: source }));
const gridAbApply = vi.fn((_playlist: string | null) => Promise.resolve({ applied: 2, skipped: 1, failed: [] as string[] }));
const notices: { text: string; failed: boolean }[] = [];
const onNotice = (text: string, failed: boolean) => { notices.push({ text, failed }); };

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  notices.length = 0;
  __setBackend({ gridAbList, gridAbUse, gridAbApply } as unknown as Backend);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  __setBackend(null);
  vi.clearAllMocks();
});

async function flush() {
  await act(async () => { await Promise.resolve(); await Promise.resolve(); });
}

async function open(protectLibrary: boolean, playlist = "") {
  act(() => root.render(<GridsView playlist={playlist} protectLibrary={protectLibrary} onNotice={onNotice} />));
  await flush();
}

const items = () => [...host.querySelectorAll("[role=listitem]")];
const button = (text: string) => [...host.querySelectorAll("button")].find((b) => b.textContent?.trim().startsWith(text))!;
const source = (item: Element, name: string) => [...item.querySelectorAll("[role=radio]")].find((b) => b.textContent === name) as HTMLButtonElement;

it("lists the tracks with two analyses, asks for all of them when no playlist is picked, and shows what the check made of each", async () => {
  await open(true);
  expect(gridAbList).toHaveBeenLastCalledWith(null, false);
  expect(items()).toHaveLength(5);
  expect(items()[0]!.textContent).toContain("Needs an ear");
  expect(items()[0]!.textContent).toContain("ear");
  expect(items()[0]!.textContent).toContain("+147 ms");
  expect(items()[3]!.textContent).toContain("-60 ms");
  expect(host.textContent).toContain("5 tracks");
});

it("asks for one playlist's tracks, and for the ones a metronome cannot tell apart when asked", async () => {
  await open(true, "7");
  expect(gridAbList).toHaveBeenLastCalledWith("7", false);
  act(() => (host.querySelectorAll("input[type=checkbox]")[0] as HTMLInputElement).click());
  await flush();
  expect(gridAbList).toHaveBeenLastCalledWith("7", true);
});

it("shows only the tracks that need an ear, which are those the check could not decide, when asked", async () => {
  await open(true);
  act(() => (host.querySelectorAll("input[type=checkbox]")[1] as HTMLInputElement).click());
  await flush();
  expect(items().map((i) => i.textContent)).toEqual([expect.stringContaining("Needs an ear"), expect.stringContaining("No verdict")]);
});

it("shows which source is in use, will not choose one while Library Protection is on, and an empty rbxport grid cannot be chosen", async () => {
  await open(true);
  expect(source(items()[1]!, "Rekordbox").getAttribute("aria-checked")).toBe("true");
  expect(source(items()[0]!, "rbxport").getAttribute("aria-checked")).toBe("true");
  act(() => source(items()[0]!, "Rekordbox").click());
  await flush();
  expect(gridAbUse).not.toHaveBeenCalled();
  expect(notices.at(-1)).toEqual({ text: expect.stringContaining("Library Protection"), failed: true });
  expect(source(items()[4]!, "rbxport").disabled).toBe(true);
});

it("chooses the other source and shows the row as it is now", async () => {
  await open(false);
  act(() => source(items()[0]!, "Rekordbox").click());
  await flush();
  expect(gridAbUse).toHaveBeenCalledWith("1", "rekordbox");
  expect(source(items()[0]!, "Rekordbox").getAttribute("aria-checked")).toBe("true");
  // The one already in use is not asked for again.
  act(() => source(items()[0]!, "Rekordbox").click());
  await flush();
  expect(gridAbUse).toHaveBeenCalledTimes(1);
});

it("applies the suggestions the check was sure of and says how many were left for the ear", async () => {
  await open(false);
  // Clear suggestions that differ from what is in use: "Clear rbxport" is on rekordbox's, "Clear rekordbox" is already rbxport's
  // = both need a change, and "Empty grid" is on rbxport's (empty) and wants rekordbox's.
  expect(button("Apply suggestions").textContent).toContain("(3)");
  act(() => button("Apply suggestions").click());
  await flush();
  expect(gridAbApply).toHaveBeenCalledWith(null);
  expect(notices.at(-1)).toEqual({ text: "Applied 2 suggestions, left 1 for the ear.", failed: false });
});

it("refuses to apply while Library Protection is on", async () => {
  await open(true);
  act(() => button("Apply suggestions").click());
  await flush();
  expect(gridAbApply).not.toHaveBeenCalled();
  expect(notices.at(-1)?.failed).toBe(true);
});

it("says there is nothing to choose when no track has two analyses", async () => {
  gridAbList.mockResolvedValueOnce([]);
  await open(true);
  expect(items()).toHaveLength(0);
  expect(host.textContent).toContain("Nothing to choose here.");
});
