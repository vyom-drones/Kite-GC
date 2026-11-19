// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// Gimbal targets on the map — per vehicle, where its camera looks (helpers/gimbalTarget): ArduPilot's point of
// interest, else the flat-ground estimate at the vehicle's home altitude. A vehicle whose camera status has
// not arrived for STALE_MS drops out, so a gimbal that stops reporting leaves no marker behind.

import { derived, readable } from 'svelte/store';
import { cameraFovs } from './telemetry';
import { vehicleHomes } from './home';
import { gimbalTarget, type GimbalTarget } from '$lib/helpers/gimbalTarget';

const STALE_MS = 5000;

/** A 1 s clock — re-evaluates staleness while no new status arrives. */
const tick = readable(Date.now(), (set) => {
  const id = setInterval(() => set(Date.now()), 1000);
  return () => clearInterval(id);
});

export const gimbalTargets = derived([cameraFovs, vehicleHomes, tick], ([fovs, homes, now]) => {
  const out = new Map<string, GimbalTarget>();
  for (const [id, fov] of fovs) {
    if (now - fov.ts > STALE_MS) continue;
    const t = gimbalTarget(fov, homes.get(id)?.alt ?? null);
    if (t) out.set(id, t);
  }
  return out as ReadonlyMap<string, GimbalTarget>;
});
