// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// Fleet video preview streams — the live tiles of the fleet video grid (backend `video::preview`).
//
// A tile on screen holds a lease (`acquirePreview`); the backend session runs while a lease is held and
// stops a few seconds after the last one is released (helpers/previewLeases), so only watched drones use
// the radio link. A session that fails or whose source dies is retried with a growing delay for as long
// as someone is watching.

import { writable } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { PreviewLeases } from '$lib/helpers/previewLeases';

export type PreviewStatus = 'starting' | 'live' | 'error';

export interface PreviewState {
  status: PreviewStatus;
  /** Local multipart MJPEG URL while live — an `<img>` source. */
  url: string | null;
  error: string | null;
  /** The RTSP URL this session pulls. */
  source: string;
}

const PREVIEW_ENDED_EVENT = 'video-preview-ended';
const IDLE_STOP_MS = 3000;
const RETRY_BASE_MS = 2000;
const RETRY_MAX_MS = 10_000;

export const previews = writable<ReadonlyMap<string, PreviewState>>(new Map());

const leases = new PreviewLeases(IDLE_STOP_MS);
const sources = new Map<string, string>();
const retryTimers = new Map<string, ReturnType<typeof setTimeout>>();
const attempts = new Map<string, number>();
let sweeper: ReturnType<typeof setInterval> | null = null;
let endedListening = false;

function setState(id: string, s: PreviewState | null): void {
  previews.update((m) => {
    const next = new Map(m);
    if (s) next.set(id, s); else next.delete(id);
    return next;
  });
}

async function start(id: string, source: string): Promise<void> {
  setState(id, { status: 'starting', url: null, error: null, source });
  try {
    const url = await invoke<string>('video_preview_start', { streamId: id, url: source });
    // Already swept while the backend waited for the first picture: its stop came first, so this session
    // would run unwatched — stop it again.
    if (!leases.running().includes(id)) {
      void invoke('video_preview_stop', { streamId: id }).catch(() => {});
      return;
    }
    // Released (or re-pointed) while starting: the idle sweep / the newer start owns it now.
    if (!leases.isHeld(id) || sources.get(id) !== source) return;
    attempts.delete(id);
    setState(id, { status: 'live', url, error: null, source });
  } catch (e) {
    if (!leases.isHeld(id) || sources.get(id) !== source) return;
    setState(id, { status: 'error', url: null, error: String(e), source });
    scheduleRetry(id);
  }
}

function scheduleRetry(id: string): void {
  const n = (attempts.get(id) ?? 0) + 1;
  attempts.set(id, n);
  clearRetry(id);
  retryTimers.set(id, setTimeout(() => {
    retryTimers.delete(id);
    const source = sources.get(id);
    if (source && leases.isHeld(id)) void start(id, source);
  }, Math.min(RETRY_MAX_MS, RETRY_BASE_MS * n)));
}

function clearRetry(id: string): void {
  const t = retryTimers.get(id);
  if (t) clearTimeout(t);
  retryTimers.delete(id);
}

function sweep(): void {
  for (const id of leases.due(Date.now())) {
    clearRetry(id);
    attempts.delete(id);
    sources.delete(id);
    setState(id, null);
    void invoke('video_preview_stop', { streamId: id }).catch(() => {});
  }
  if (leases.running().length === 0 && sweeper) {
    clearInterval(sweeper);
    sweeper = null;
  }
}

function ensureRunning(): void {
  sweeper ??= setInterval(sweep, 1000);
  if (endedListening) return;
  endedListening = true;
  void listen<{ streamId: string }>(PREVIEW_ENDED_EVENT, (e) => {
    const id = e.payload.streamId;
    if (!leases.isHeld(id)) return;
    const source = sources.get(id) ?? '';
    setState(id, { status: 'error', url: null, error: 'Stream lost — reconnecting', source });
    scheduleRetry(id);
  });
}

/** Watch preview stream `id` (`"L1:S7:preview"`) from RTSP `source`. Returns the release function. */
export function acquirePreview(id: string, source: string): () => void {
  ensureRunning();
  const moved = sources.has(id) && sources.get(id) !== source;
  sources.set(id, source);
  if (leases.acquire(id) || moved) {
    attempts.delete(id);
    clearRetry(id);
    void start(id, source);
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    leases.release(id, Date.now());
  };
}
