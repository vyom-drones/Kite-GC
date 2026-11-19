// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// Gimbal target — the ground point a vehicle's camera looks at (CAMERA_FOV_STATUS, backend `camera_fov`).
//
// ArduPilot's own point of interest (its mount ray-casts the line of sight into the FC's terrain database)
// is used when the FC has one. Without terrain data on the FC that point is missing, and the target is
// ESTIMATED here: the line of sight from the camera position, at its pitch and yaw, meets flat ground at the
// vehicle's home altitude. A gimbal at or near the horizon has no estimate (the ground point would be
// arbitrarily far). No runtime imports: unit-tested under Node (tools/tests/).

/** The backend payload (camera_fov::CameraFov). */
export interface CameraFov {
  camLat: number | null;
  camLon: number | null;
  camAltMsl: number | null;
  poiLat: number | null;
  poiLon: number | null;
  poiAltMsl: number | null;
  pitchDeg: number | null;
  yawDeg: number | null;
  hfovDeg: number | null;
  vfovDeg: number | null;
}

export type TargetSource = 'ardupilot' | 'estimate';

export interface GimbalTarget {
  lat: number;
  lon: number;
  /** m MSL; null when unknown. */
  altMsl: number | null;
  source: TargetSource;
  camLat: number;
  camLon: number;
  camAltMsl: number | null;
  /** Horizontal distance camera → target (m). */
  groundDistM: number;
  /** Line-of-sight distance (m); null without both altitudes. */
  slantM: number | null;
}

/** The line of sight must point at least this far below the horizon for an estimate. */
export const MIN_DOWN_PITCH_DEG = 2;
/** Farther than this, a flat-ground estimate means nothing (the terrain decides) — no estimate. */
export const MAX_ESTIMATE_M = 5000;

const EARTH_R = 6_371_000;
const RAD = Math.PI / 180;

/** Great-circle distance (m). */
export function distanceM(lat1: number, lon1: number, lat2: number, lon2: number): number {
  const dLat = (lat2 - lat1) * RAD;
  const dLon = (lon2 - lon1) * RAD;
  const a = Math.sin(dLat / 2) ** 2 + Math.cos(lat1 * RAD) * Math.cos(lat2 * RAD) * Math.sin(dLon / 2) ** 2;
  return 2 * EARTH_R * Math.asin(Math.min(1, Math.sqrt(a)));
}

/** The point `distM` from (lat, lon) along `bearingDeg` (deg true). */
export function destination(lat: number, lon: number, bearingDeg: number, distM: number): [number, number] {
  const d = distM / EARTH_R;
  const b = bearingDeg * RAD;
  const p1 = lat * RAD;
  const p2 = Math.asin(Math.sin(p1) * Math.cos(d) + Math.cos(p1) * Math.sin(d) * Math.cos(b));
  const l2 = lon * RAD + Math.atan2(Math.sin(b) * Math.sin(d) * Math.cos(p1), Math.cos(d) - Math.sin(p1) * Math.sin(p2));
  return [p2 / RAD, ((l2 / RAD + 540) % 360) - 180];
}

/** "lat, lon" in decimal degrees, 7 places (~1 cm) — the form the clipboard copy and coordinate search take. */
export function formatLatLon(lat: number, lon: number): string {
  return `${lat.toFixed(7)}, ${lon.toFixed(7)}`;
}

type Translate = (key: string, opts?: { values: Record<string, string | number> }) => string;

/** The hover text of a target (2D tooltip and 3D popup alike), one string per line: who / how far / source,
 *  then lat, lon (+ altitude), then the copy hint — or "copied" right after a click. */
export function gimbalTipLines(tr: Translate, name: string, g: GimbalTarget, copied: boolean): string[] {
  const dist = tr('gimbal.dist', { values: { d: Math.round(g.groundDistM), s: g.slantM != null ? Math.round(g.slantM) : '—' } });
  const source = tr(g.source === 'ardupilot' ? 'gimbal.sourceArdupilot' : 'gimbal.sourceEstimate');
  const alt = g.altMsl != null ? ` · ${tr('gimbal.altMsl', { values: { a: Math.round(g.altMsl) } })}` : '';
  return [
    `${tr('gimbal.target', { values: { name } })} · ${dist} · ${source}`,
    `${formatLatLon(g.lat, g.lon)}${alt}`,
    tr(copied ? 'gimbal.copied' : 'gimbal.clickToCopy'),
  ];
}

/** Where the camera looks: ArduPilot's point, else the flat-ground estimate at `groundAltMsl`; null when
 *  neither is possible (no camera position, gimbal at/above the horizon, too far, camera below ground). */
export function gimbalTarget(fov: CameraFov, groundAltMsl: number | null): GimbalTarget | null {
  if (fov.camLat == null || fov.camLon == null) return null;
  const cam = { camLat: fov.camLat, camLon: fov.camLon, camAltMsl: fov.camAltMsl };
  if (fov.poiLat != null && fov.poiLon != null) {
    const groundDistM = distanceM(fov.camLat, fov.camLon, fov.poiLat, fov.poiLon);
    const slantM = fov.camAltMsl != null && fov.poiAltMsl != null
      ? Math.hypot(groundDistM, fov.camAltMsl - fov.poiAltMsl)
      : null;
    return { lat: fov.poiLat, lon: fov.poiLon, altMsl: fov.poiAltMsl, source: 'ardupilot', ...cam, groundDistM, slantM };
  }
  if (fov.pitchDeg == null || fov.yawDeg == null || fov.camAltMsl == null || groundAltMsl == null) return null;
  if (fov.pitchDeg > -MIN_DOWN_PITCH_DEG) return null;
  const height = fov.camAltMsl - groundAltMsl;
  if (height <= 0) return null;
  const groundDistM = height / Math.tan(-fov.pitchDeg * RAD);
  if (groundDistM > MAX_ESTIMATE_M) return null;
  const [lat, lon] = destination(fov.camLat, fov.camLon, fov.yawDeg, groundDistM);
  return {
    lat, lon, altMsl: groundAltMsl, source: 'estimate', ...cam, groundDistM, slantM: Math.hypot(groundDistM, height),
  };
}
