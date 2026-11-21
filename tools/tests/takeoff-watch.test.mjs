// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom
//
// Unit tests for src/lib/helpers/takeoffWatch.ts (transpiled with Vite's esbuild).
//   node --test tools/tests/

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import esbuild from 'esbuild';

const src = readFileSync(fileURLToPath(new URL('../../src/lib/helpers/takeoffWatch.ts', import.meta.url)), 'utf8');
const { code } = esbuild.transformSync(src, { loader: 'ts', format: 'esm' });
const { takeoffWatchVerdict, TAKEOFF_WATCH_MS, TAKEOFF_WATCH_GRACE_MS } = await import(
  'data:text/javascript;base64,' + Buffer.from(code).toString('base64')
);

test('a vehicle that stays armed for the whole window passes', () => {
  assert.equal(takeoffWatchVerdict(true, true, 100), 'watching');
  assert.equal(takeoffWatchVerdict(true, true, TAKEOFF_WATCH_MS - 1), 'watching');
  assert.equal(takeoffWatchVerdict(true, true, TAKEOFF_WATCH_MS), 'stayedArmed');
});

test('armed, then disarmed (auto-disarm 150 ms after the takeoff) is caught at once', () => {
  assert.equal(takeoffWatchVerdict(false, true, 150), 'disarmed');
});

test('a stale pre-arm "disarmed" is tolerated during the grace, not after', () => {
  assert.equal(takeoffWatchVerdict(false, false, 500), 'watching');
  assert.equal(takeoffWatchVerdict(false, false, TAKEOFF_WATCH_GRACE_MS - 1), 'watching');
  assert.equal(takeoffWatchVerdict(false, false, TAKEOFF_WATCH_GRACE_MS), 'disarmed');
  assert.equal(takeoffWatchVerdict(false, false, TAKEOFF_WATCH_MS), 'disarmed', 'never armed at all');
});
