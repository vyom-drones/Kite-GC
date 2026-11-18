// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// Fleet video, phase 1 — the video views follow the ACTIVE vehicle. Each vehicle's RTSP URL comes from the
// fleet template or its override (helpers/fleetVideoUrl); when the active vehicle changes, the running RTSP
// source moves to that vehicle's main stream. Only the watched vehicle streams, so the radio link carries
// one video at a time.
//
// A vehicle without a URL pauses the feed rather than leaving the previous aircraft's picture on screen
// under the new one's selection; the next vehicle that has a URL resumes it. Only an RTSP source follows —
// a local camera is never replaced — and only with the fleet features on.

import { writable, derived, get } from 'svelte/store';
import { listen } from '@tauri-apps/api/event';
import { activeVehicleId, vehicles, type VehicleSummary } from './vehicles';
import { fleetEnabled } from './fleetMode';
import { videoState, followRtspUrl, stopVideo } from './video';
import {
  parseFleetVideoConfig, resolveStreamUrl, overrideKey, followAction, type FleetVideoConfig, type DiscoveredStream,
} from '$lib/helpers/fleetVideoUrl';

const STORAGE_KEY = 'kite-gc-fleet-video';
/** Collapse rapid selection changes (clicking through the fleet) into one stream switch. */
const SWITCH_DEBOUNCE_MS = 300;

function load(): FleetVideoConfig {
  try {
    return parseFleetVideoConfig(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? 'null'));
  } catch {
    return parseFleetVideoConfig(null);
  }
}

export const fleetVideoConfig = writable<FleetVideoConfig>(load());
fleetVideoConfig.subscribe((c) => {
  try { localStorage.setItem(STORAGE_KEY, JSON.stringify(c)); } catch { /* private mode / blocked storage */ }
});

/** Streams each vehicle's camera advertised over MAVLink (`telemetry-video-stream`), by vehicle id. */
export const discoveredStreams = writable<ReadonlyMap<string, DiscoveredStream[]>>(new Map());

export interface VehicleStreams {
  main: string | null;
  preview: string | null;
}

/** Every known vehicle's main / preview URL (override → template → advertised; null = none). */
export const vehicleStreams = derived([fleetVideoConfig, vehicles, discoveredStreams], ([cfg, vs, found]) =>
  new Map([...vs.values()].map((v) => {
    const d = found.get(v.vehicleId) ?? [];
    return [v.vehicleId, { main: resolveStreamUrl(cfg, v, 'main', d), preview: resolveStreamUrl(cfg, v, 'preview', d) }] as const;
  })));

/** The URL the video views follow now: the active vehicle's main stream; null when not following. */
export const followedStreamUrl = derived(
  [fleetVideoConfig, vehicleStreams, activeVehicleId, fleetEnabled],
  ([cfg, streams, id, on]): string | null => {
    if (!on || !cfg.followActive || !id) return null;
    return streams.get(id)?.main ?? null;
  },
);

export function setFleetVideoFollow(followActive: boolean): void {
  fleetVideoConfig.update((c) => ({ ...c, followActive }));
}

export function setFleetVideoTemplate(template: string): void {
  fleetVideoConfig.update((c) => ({ ...c, template: template.trim() }));
}

/** Set (or with '' clear) a vehicle's own URL. */
export function setVehicleVideoOverride(v: VehicleSummary, url: string): void {
  const key = overrideKey(v);
  const trimmed = url.trim();
  fleetVideoConfig.update((c) => {
    const overrides = { ...c.overrides };
    if (trimmed) overrides[key] = trimmed; else delete overrides[key];
    return { ...c, overrides };
  });
}

export function vehicleVideoOverride(cfg: FleetVideoConfig, v: VehicleSummary): string {
  return cfg.overrides[overrideKey(v)] ?? '';
}

let started = false;
let timer: ReturnType<typeof setTimeout> | null = null;
/** The feed was paused because the active vehicle has no stream — the next one with a URL resumes it. */
let pausedForVehicle = false;

function follow(url: string | null): void {
  const st = get(videoState);
  const following = get(fleetVideoConfig).followActive && get(fleetEnabled) && get(activeVehicleId) !== null;
  switch (followAction({ kind: st.kind, enabled: st.enabled, url, paused: pausedForVehicle, following })) {
    case 'switch':
      void followRtspUrl(url!);
      break;
    case 'resume':
      pausedForVehicle = false;
      void followRtspUrl(url!, true);
      break;
    case 'pause':
      stopVideo();
      pausedForVehicle = true;
      break;
    case 'none':
      break;
  }
}

/** Start following (idempotent) — +page at startup. */
export function initFleetVideo(): void {
  if (started) return;
  started = true;
  followedStreamUrl.subscribe((url) => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => { timer = null; follow(url); }, SWITCH_DEBOUNCE_MS);
  });
  // Only a pause made here resumes on its own: video the operator starts (or stops) themselves ends it.
  videoState.subscribe((s) => { if (s.enabled) pausedForVehicle = false; });
  // Camera discovery: one event per advertised stream; the same camera + stream id replaces the older entry.
  void listen<DiscoveredStream & { vehicleId?: string }>('telemetry-video-stream', (e) => {
    const { vehicleId, ...stream } = e.payload;
    if (!vehicleId) return;
    discoveredStreams.update((m) => {
      const next = new Map(m);
      const list = (next.get(vehicleId) ?? []).filter((s) => !(s.compid === stream.compid && s.streamId === stream.streamId));
      next.set(vehicleId, [...list, stream]);
      return next;
    });
  });
  // Vehicles the backend dropped forget their streams (they are asked again on rediscovery).
  vehicles.subscribe((known) => {
    if ([...get(discoveredStreams).keys()].some((id) => !known.has(id))) {
      discoveredStreams.update((m) => new Map([...m].filter(([id]) => known.has(id))));
    }
  });
}
