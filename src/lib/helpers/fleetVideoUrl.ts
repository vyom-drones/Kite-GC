// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// Fleet video — which RTSP URL belongs to which vehicle.
//
// Every drone's companion serves its camera over RTSP (a `main` stream for big views, a low-res `preview`
// for tiles). Kite pulls the stream of the vehicle being watched, so only what is on screen uses the radio
// link. The URL comes from a per-vehicle override, else from one template for the whole fleet:
//
//   rtsp://10.10.0.{sysid}:8554/{stream}   →   rtsp://10.10.0.7:8554/main
//
// Placeholders: {sysid} {link} {name} {fcuid} {stream} (= main | preview). An override without {stream}
// is the main stream only. With neither, a stream the drone's camera advertises over MAVLink
// (VIDEO_STREAM_INFORMATION) is used. No runtime imports: unit-tested under Node (tools/tests/).

export type StreamKind = 'main' | 'preview';

export interface FleetVideoConfig {
  /** The active vehicle's main stream drives the video views (switching with the Fleet selection). */
  followActive: boolean;
  /** URL template for every vehicle without an override; '' = none. */
  template: string;
  /** Per-vehicle URLs, keyed by `overrideKey` (stable across sessions). */
  overrides: Record<string, string>;
}

export const DEFAULT_FLEET_VIDEO: FleetVideoConfig = { followActive: true, template: '', overrides: {} };

/** What a URL can be resolved from — a subset of `VehicleSummary`. */
export interface VideoVehicle {
  vehicleId: string;
  linkId: number;
  sysid: number;
  name: string;
  fcUid: string | null;
}

/** The override slot of a vehicle: its FC hardware id when known (survives reconnects and sysid changes),
 *  else its system id. */
export function overrideKey(v: Pick<VideoVehicle, 'fcUid' | 'sysid'>): string {
  return v.fcUid ? `fc:${v.fcUid}` : `sys:${v.sysid}`;
}

/** Fill a template's placeholders for `v` / `kind`. */
export function fillTemplate(template: string, v: VideoVehicle, kind: StreamKind): string {
  return template.replace(/\{(sysid|link|name|fcuid|stream)\}/g, (_m, key: string) => {
    switch (key) {
      case 'sysid': return String(v.sysid);
      case 'link': return String(v.linkId);
      case 'name': return encodeURIComponent(v.name);
      case 'fcuid': return v.fcUid ?? '';
      default: return kind;
    }
  });
}

/** A stream a vehicle's camera advertises (backend `video_streams::VideoStream`). */
export interface DiscoveredStream {
  compid: number;
  streamId: number;
  kind: string;
  name: string;
  uri: string;
  width: number;
  height: number;
}

const MAIN_NAME = /\b(main|high|hd|primary)\b/i;
const PREVIEW_NAME = /\b(preview|sub|low|thumb|thumbnail|lowres)\b/i;

/** Which advertised RTSP stream is `kind`: named main / preview first, else the largest (main) and smallest
 *  (preview) resolution. A camera with a single stream has no preview. */
export function pickDiscovered(streams: readonly DiscoveredStream[], kind: StreamKind): string | null {
  const rtsp = streams.filter((s) => s.kind === 'rtsp' && /^rtsps?:\/\//i.test(s.uri));
  if (rtsp.length === 0) return null;
  const named = rtsp.find((s) => (kind === 'main' ? MAIN_NAME : PREVIEW_NAME).test(s.name));
  if (named) return named.uri;
  if (kind === 'preview' && rtsp.length < 2) return null;
  const bySize = [...rtsp].sort((a, b) => a.width * a.height - b.width * b.height || a.streamId - b.streamId);
  const pick = kind === 'main' ? bySize[bySize.length - 1] : bySize[0];
  // A preview that is also the only named main stream is no preview.
  return kind === 'preview' && MAIN_NAME.test(pick.name) ? null : pick.uri;
}

/** The URL of vehicle `v`'s `kind` stream, or null when it has none: its override, else the template, else
 *  what its camera advertises. */
export function resolveStreamUrl(
  cfg: FleetVideoConfig,
  v: VideoVehicle,
  kind: StreamKind,
  discovered: readonly DiscoveredStream[] = [],
): string | null {
  const override = cfg.overrides[overrideKey(v)]?.trim();
  if (override) {
    if (kind === 'preview' && !override.includes('{stream}')) return null;
    return fillTemplate(override, v, kind);
  }
  const template = cfg.template.trim();
  if (template) return fillTemplate(template, v, kind);
  return pickDiscovered(discovered, kind);
}

/** Parse a persisted config, keeping defaults for anything missing or malformed. */
export function parseFleetVideoConfig(raw: unknown): FleetVideoConfig {
  if (!raw || typeof raw !== 'object') return { ...DEFAULT_FLEET_VIDEO, overrides: {} };
  const r = raw as Record<string, unknown>;
  const overrides: Record<string, string> = {};
  if (r.overrides && typeof r.overrides === 'object') {
    for (const [k, val] of Object.entries(r.overrides as Record<string, unknown>)) {
      if (typeof val === 'string' && val.trim()) overrides[k] = val;
    }
  }
  return {
    followActive: typeof r.followActive === 'boolean' ? r.followActive : DEFAULT_FLEET_VIDEO.followActive,
    template: typeof r.template === 'string' ? r.template : '',
    overrides,
  };
}

/** What the follower does when the followed URL changes:
 *  `switch` — point the RTSP source at the URL (restarts a live feed);
 *  `resume` — the same, and start the feed the follower had paused;
 *  `pause`  — the active vehicle has no stream: stop the feed instead of showing another aircraft's;
 *  `none`   — not an RTSP source, or nothing to do. */
export type FollowAction = 'switch' | 'resume' | 'pause' | 'none';

export function followAction(s: {
  kind: string;
  enabled: boolean;
  url: string | null;
  /** The follower paused the feed earlier (a vehicle without a stream). */
  paused: boolean;
  /** Following is on, with the fleet features and an active vehicle. */
  following: boolean;
}): FollowAction {
  if (s.kind !== 'rtsp') return 'none';
  if (s.url) return s.paused ? 'resume' : 'switch';
  return s.enabled && s.following ? 'pause' : 'none';
}
