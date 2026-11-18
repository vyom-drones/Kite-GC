// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom
//
// Unit tests for src/lib/helpers/fleetModelSprites.ts — no DOM, no extra packages: the helper is
// transpiled with the esbuild that Vite already ships and run with Node's built-in test runner.
//   node --test tools/tests/

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import esbuild from 'esbuild';

const src = readFileSync(fileURLToPath(new URL('../../src/lib/helpers/fleetModelSprites.ts', import.meta.url)), 'utf8');
const { code } = esbuild.transformSync(src, { loader: 'ts', format: 'esm' });
const { FleetModelSprites, RasterBudget, quantize, TILT_STEP_DEG } = await import(
  'data:text/javascript;base64,' + Buffer.from(code).toString('base64')
);

/** A canvas whose 2D context records every call. */
function fakeCanvas(size = 32) {
  const calls = [];
  const ctx = new Proxy({}, { get: (_t, name) => (...args) => calls.push([name, ...args]) });
  return { width: size, height: size, calls, getContext: () => ctx };
}

function setup(budget = new RasterBudget(8, 8)) {
  const renders = [];
  const render = (_ctx, _mesh, size, h, p, r) => renders.push({ size, h, p, r });
  const sprites = new FleetModelSprites(render, (size) => fakeCanvas(size), budget);
  const draw = (target, { id = 'L1:S2', type = 0, h = 0, p = 0, r = 0, color = '#ff0000', t = 0 } = {}) =>
    sprites.draw(id, target, {}, type, h, p, r, color, [1, 0, 0], 0.5, t);
  return { sprites, renders, draw };
}

const rotations = (c) => c.calls.filter(([n]) => n === 'rotate').map(([, a]) => Math.round((a * 180) / Math.PI));

test('first draw rasterises once at heading 0 and rotates the sprite to the heading', () => {
  const { renders, draw } = setup();
  const target = fakeCanvas();
  draw(target, { h: 90, p: 3, r: -7 });
  assert.deepEqual(renders, [{ size: 32, h: 0, p: 5, r: -5 }]);
  assert.deepEqual(rotations(target), [90]);
});

test('a heading change only rotates — no new raster', () => {
  const { renders, draw } = setup();
  const target = fakeCanvas();
  draw(target, { h: 10 });
  draw(target, { h: 200 });
  assert.equal(renders.length, 1);
  assert.deepEqual(rotations(target), [10, 200]);
});

test('an unchanged view does no canvas work at all', () => {
  const { draw } = setup();
  const target = fakeCanvas();
  draw(target, { h: 45.2 });
  const before = target.calls.length;
  draw(target, { h: 44.8 }); // rounds to the same degree
  assert.equal(target.calls.length, before);
});

test('tilt inside the same step reuses the sprite; a new step re-rasterises', () => {
  const { renders, draw } = setup();
  const target = fakeCanvas();
  draw(target, { p: 1 });
  draw(target, { p: 2 });
  assert.equal(renders.length, 1);
  draw(target, { p: 1 + TILT_STEP_DEG });
  assert.equal(renders.length, 2);
  assert.equal(renders[1].p, TILT_STEP_DEG);
});

test('the tilt budget caps re-rasters fleet-wide and refills over time', () => {
  const { renders, draw } = setup(new RasterBudget(2, 2));
  const a = fakeCanvas(), b = fakeCanvas(), c = fakeCanvas();
  draw(a, { id: 'A', t: 0 }); draw(b, { id: 'B', t: 0 }); draw(c, { id: 'C', t: 0 }); // first rasters: exempt
  assert.equal(renders.length, 3);
  draw(a, { id: 'A', p: 20, t: 0 });
  draw(b, { id: 'B', p: 20, t: 0 });
  draw(c, { id: 'C', p: 20, t: 0 }); // budget (2) spent → keeps its old sprite
  assert.equal(renders.length, 5);
  draw(c, { id: 'C', p: 20, t: 1000 }); // a second later the bucket has refilled
  assert.equal(renders.length, 6);
});

test('a new colour, type or size re-rasterises even with no budget left', () => {
  const { renders, draw } = setup(new RasterBudget(0, 0));
  const target = fakeCanvas();
  draw(target, { color: '#ff0000' });
  draw(target, { color: '#00ff00' });
  draw(target, { color: '#00ff00', type: 1 });
  assert.equal(renders.length, 3);
  draw(target, { color: '#00ff00', type: 1, p: 30 }); // tilt only, budget empty → no raster
  assert.equal(renders.length, 3);
});

test('a replaced marker canvas (setIcon) is redrawn from the cached sprite', () => {
  const { renders, draw } = setup();
  draw(fakeCanvas(), { h: 30 });
  const fresh = fakeCanvas();
  draw(fresh, { h: 30 });
  assert.equal(renders.length, 1);
  assert.deepEqual(rotations(fresh), [30]);
});

test('forget drops the sprite', () => {
  const { sprites, renders, draw } = setup();
  draw(fakeCanvas());
  sprites.forget('L1:S2');
  draw(fakeCanvas());
  assert.equal(renders.length, 2);
});

test('quantize rounds to the step and never yields -0', () => {
  assert.equal(quantize(7.4, 5), 5);
  assert.equal(quantize(-7.6, 5), -10);
  assert.ok(Object.is(quantize(-0.4, 5), 0));
});

test('RasterBudget starts full, spends one per take and refills at perSec', () => {
  const b = new RasterBudget(4, 2);
  assert.equal(b.take(0), true);
  assert.equal(b.take(0), true);
  assert.equal(b.take(0), false);
  assert.equal(b.take(250), true); // 0.25 s × 4/s = 1 token
  assert.equal(b.take(250), false);
});
