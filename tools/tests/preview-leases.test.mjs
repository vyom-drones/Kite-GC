// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom
//
// Unit tests for src/lib/helpers/previewLeases.ts (transpiled with Vite's esbuild).
//   node --test tools/tests/

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import esbuild from 'esbuild';

const src = readFileSync(fileURLToPath(new URL('../../src/lib/helpers/previewLeases.ts', import.meta.url)), 'utf8');
const { code } = esbuild.transformSync(src, { loader: 'ts', format: 'esm' });
const { PreviewLeases } = await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'));

test('the first lease starts the stream, further leases do not', () => {
  const l = new PreviewLeases(3000);
  assert.equal(l.acquire('a'), true);
  assert.equal(l.acquire('a'), false);
  assert.equal(l.isHeld('a'), true);
});

test('the stream stops only after the last lease has been idle for idleMs', () => {
  const l = new PreviewLeases(3000);
  l.acquire('a'); l.acquire('a');
  l.release('a', 0);
  assert.deepEqual(l.due(10_000), [], 'one viewer is still left');
  l.release('a', 1000);
  assert.equal(l.isHeld('a'), false);
  assert.deepEqual(l.due(3999), []);
  assert.deepEqual(l.due(4000), ['a']);
  assert.deepEqual(l.due(9000), [], 'reported once');
  assert.deepEqual(l.running(), []);
});

test('re-acquiring while idling keeps the running stream (no restart)', () => {
  const l = new PreviewLeases(3000);
  l.acquire('a');
  l.release('a', 0);
  assert.equal(l.acquire('a'), false, 'still running — scrolling back must not reconnect');
  assert.deepEqual(l.due(10_000), []);
  l.release('a', 20_000);
  assert.deepEqual(l.due(23_000), ['a']);
  assert.equal(l.acquire('a'), true, 'after the stop a new lease starts it again');
});

test('streams are tracked independently', () => {
  const l = new PreviewLeases(1000);
  l.acquire('a'); l.acquire('b');
  l.release('a', 0);
  assert.deepEqual(l.due(1000), ['a']);
  assert.equal(l.isHeld('b'), true);
  assert.deepEqual(l.running(), ['b']);
});

test('a release without a lease is ignored', () => {
  const l = new PreviewLeases(1000);
  l.release('x', 0);
  assert.deepEqual(l.due(5000), []);
  assert.deepEqual(l.running(), []);
});
