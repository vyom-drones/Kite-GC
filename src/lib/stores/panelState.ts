// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Marc Hoffmann (b14ckyy)

// Per-panel UI view state (open tab, compact vs advanced) in a module-level store, so it survives panel
// switches (inactive panels unmount) and, via localStorage, restarts.

import { writable } from 'svelte/store';

const SETTINGS_TABS = ['interface', 'map', 'data'] as const;
export type SettingsTab = (typeof SETTINGS_TABS)[number];
const FLEET_VIDEO_TILES = ['s', 'm', 'l'] as const;
export type FleetVideoTile = (typeof FLEET_VIDEO_TILES)[number];
const FLEET_VIDEO_VIEWS = ['all', 'selected'] as const;
export type FleetVideoView = (typeof FLEET_VIDEO_VIEWS)[number];

export interface PanelState {
  /** Radar panel: compact (info) vs advanced (full) view. */
  radarCompact: boolean;
  /** Settings panel: which tab was last open. */
  settingsTab: SettingsTab;
  /** Fleet video grid: tile size. */
  fleetVideoTile: FleetVideoTile;
  /** Fleet video grid: every drone, or only the group-selected ones (a strip above the map + main video). */
  fleetVideoView: FleetVideoView;
}

const DEFAULTS: PanelState = {
  radarCompact: false,
  settingsTab: 'interface',
  fleetVideoTile: 'm',
  fleetVideoView: 'all',
};

const STORAGE_KEY = 'kite-gc-panelstate';

function load(): PanelState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) {
      const merged = { ...DEFAULTS, ...(JSON.parse(raw) as Partial<PanelState>) };
      // A tab that no longer exists (or a hand-edited value) falls back to the first one.
      if (!SETTINGS_TABS.includes(merged.settingsTab)) merged.settingsTab = DEFAULTS.settingsTab;
      if (!FLEET_VIDEO_TILES.includes(merged.fleetVideoTile)) merged.fleetVideoTile = DEFAULTS.fleetVideoTile;
      if (!FLEET_VIDEO_VIEWS.includes(merged.fleetVideoView)) merged.fleetVideoView = DEFAULTS.fleetVideoView;
      return merged;
    }
  } catch {
    // ignore parse errors → defaults
  }
  return { ...DEFAULTS };
}

function createPanelState() {
  const { subscribe, update } = writable<PanelState>(load());
  return {
    subscribe,
    /** Patch one or more fields and persist. */
    patch(partial: Partial<PanelState>) {
      update((current) => {
        const next = { ...current, ...partial };
        try { localStorage.setItem(STORAGE_KEY, JSON.stringify(next)); } catch { /* ignore */ }
        return next;
      });
    },
  };
}

export const panelState = createPanelState();
