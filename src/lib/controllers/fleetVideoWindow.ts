// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// The separate fleet video window (route /fleet-video, backend `fleet_video_window_open`) — a video wall for a
// second display while the main window keeps the map. That window is its own WebView with no telemetry or
// fleet state, so the MAIN window sends it the tile model (helpers/fleetVideoTiles) whenever that changes,
// at most twice a second. The window pulls the preview streams itself (it is a session owner of its own) and
// makes a drone active through the backend directly, so the main window follows.

import { writable, get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { listen, emitTo, type UnlistenFn } from '@tauri-apps/api/event';
import { vehicles, activeVehicleId } from '$lib/stores/vehicles';
import { allTelemetry } from '$lib/stores/telemetry';
import { selectedVehicleIds } from '$lib/stores/fleetSelection';
import { vehicleStreams } from '$lib/stores/fleetVideo';
import { buildTiles } from '$lib/helpers/fleetVideoTiles';
import {
  FLEET_VIDEO_WINDOW, MODEL_EVENT, READY_EVENT, CLOSED_EVENT, type FleetVideoModel,
} from '$lib/helpers/fleetVideoWindowEvents';

const PUSH_INTERVAL_MS = 500;

/** True while the fleet video window is open. */
export const fleetVideoWindowOpen = writable(false);

let unlisteners: UnlistenFn[] = [];
let unsubs: (() => void)[] = [];
let timer: ReturnType<typeof setInterval> | null = null;
let dirty = true;

function model(): FleetVideoModel {
  const streams = get(vehicleStreams);
  return {
    tiles: buildTiles(get(vehicles), get(allTelemetry), (id) => streams.get(id)?.preview ?? null, get(selectedVehicleIds)),
    activeId: get(activeVehicleId),
  };
}

function push(): void {
  dirty = false;
  void emitTo(FLEET_VIDEO_WINDOW, MODEL_EVENT, model()).catch(() => {});
}

function startFeeding(): void {
  if (timer) return;
  const mark = () => { dirty = true; };
  unsubs = [vehicles, allTelemetry, selectedVehicleIds, vehicleStreams, activeVehicleId].map((s) => s.subscribe(mark));
  timer = setInterval(() => { if (dirty) push(); }, PUSH_INTERVAL_MS);
}

function stopFeeding(): void {
  if (timer) clearInterval(timer);
  timer = null;
  for (const u of unsubs) u();
  unsubs = [];
}

/** Listen for the window's lifecycle (idempotent) — +page at startup. */
export async function initFleetVideoWindow(): Promise<void> {
  if (unlisteners.length) return;
  unlisteners = [
    // The window's page is up (first open, or a reload of it): it gets the model at once.
    await listen(READY_EVENT, () => {
      fleetVideoWindowOpen.set(true);
      startFeeding();
      push();
    }),
    await listen(CLOSED_EVENT, () => {
      fleetVideoWindowOpen.set(false);
      stopFeeding();
    }),
  ];
}

export async function openFleetVideoWindow(): Promise<void> {
  await invoke('fleet_video_window_open');
}

export function closeFleetVideoWindow(): void {
  void invoke('fleet_video_window_close').catch(() => {});
}
