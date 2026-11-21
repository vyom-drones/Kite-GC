// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

// Takeoff watch — did a vehicle stay armed after its takeoff was accepted?
//
// An accepted NAV_TAKEOFF does not mean the vehicle leaves: armed on the ground for DISARM_DELAY (ArduCopter,
// 10 s by default) it disarms itself, even a few ms after the takeoff arrived. That is what a group Arm
// followed by a staggered group Takeoff runs into. The group runner watches each vehicle for a short window
// after its takeoff and, on a disarm, arms and takes off once more. No runtime imports: unit-tested under
// Node (tools/tests/).

/** How long a vehicle is watched after its takeoff (ms). */
export const TAKEOFF_WATCH_MS = 4000;
/** Telemetry may still show the pre-arm "disarmed" this long after the arm (inactive fleet vehicles report
 *  their heartbeat at 1 Hz) — a disarmed state counts only after an armed one was seen, or after this. */
export const TAKEOFF_WATCH_GRACE_MS = 2000;

export type TakeoffWatchVerdict = 'disarmed' | 'watching' | 'stayedArmed';

/** One look at the vehicle: `sawArmed` = an armed state was seen since the takeoff was sent. */
export function takeoffWatchVerdict(armedNow: boolean, sawArmed: boolean, elapsedMs: number): TakeoffWatchVerdict {
  if (!armedNow && (sawArmed || elapsedMs >= TAKEOFF_WATCH_GRACE_MS)) return 'disarmed';
  return elapsedMs >= TAKEOFF_WATCH_MS ? 'stayedArmed' : 'watching';
}
