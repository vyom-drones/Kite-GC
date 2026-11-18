// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// The event channel between the main window and the separate fleet video window (controllers/
// fleetVideoWindow ↔ components/video/FleetVideoWindow). No runtime imports: the video window must not pull
// the app's stores in.

import type { FleetVideoTileModel } from './fleetVideoTiles';

/** The fleet video window's label (backend `FLEET_VIDEO_LABEL`). */
export const FLEET_VIDEO_WINDOW = 'fleet-video';
/** main → window: the tiles to show (`FleetVideoModel`). */
export const MODEL_EVENT = 'fleet-video-model';
/** window → main: the page is up and wants the model. */
export const READY_EVENT = 'fleet-video-window-ready';
/** backend → main: the window is gone. */
export const CLOSED_EVENT = 'fleet-video-window-closed';

export interface FleetVideoModel {
  tiles: FleetVideoTileModel[];
  activeId: string | null;
}
