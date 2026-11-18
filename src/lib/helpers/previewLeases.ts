// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// Fleet video — who is watching which preview stream (stores/videoPreviews).
//
// Every visible tile holds a lease on its stream; the stream runs while at least one lease is held. The
// last release does not stop it at once: a grid that scrolls, or a tile that re-renders, would otherwise
// tear the RTSP session down and set it up again within moments. A stream idle for `idleMs` is stopped.
// Pure bookkeeping (the clock is passed in) — unit-tested under Node (tools/tests/).

export class PreviewLeases {
  private readonly refs = new Map<string, number>();
  private readonly idleSince = new Map<string, number>();

  constructor(readonly idleMs = 3000) {}

  /** Take a lease. True when the stream must be started (it is neither held nor still idling). */
  acquire(id: string): boolean {
    const n = (this.refs.get(id) ?? 0) + 1;
    this.refs.set(id, n);
    const wasIdling = this.idleSince.delete(id);
    return n === 1 && !wasIdling;
  }

  /** Give a lease back; the last one starts the idle countdown. */
  release(id: string, nowMs: number): void {
    const n = this.refs.get(id);
    if (n === undefined) return;
    if (n > 1) {
      this.refs.set(id, n - 1);
      return;
    }
    this.refs.delete(id);
    this.idleSince.set(id, nowMs);
  }

  /** Streams idle for at least `idleMs`: stop them now (each is returned once). */
  due(nowMs: number): string[] {
    const out: string[] = [];
    for (const [id, since] of this.idleSince) {
      if (nowMs - since >= this.idleMs) {
        out.push(id);
        this.idleSince.delete(id);
      }
    }
    return out;
  }

  isHeld(id: string): boolean {
    return (this.refs.get(id) ?? 0) > 0;
  }

  /** Streams still running: held, or idling before their stop. */
  running(): string[] {
    return [...new Set([...this.refs.keys(), ...this.idleSince.keys()])];
  }
}
