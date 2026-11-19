// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom
//
// Unit tests for src/lib/helpers/gimbalTarget.ts (transpiled with Vite's esbuild).
//   node --test tools/tests/

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import esbuild from 'esbuild';

const src = readFileSync(fileURLToPath(new URL('../../src/lib/helpers/gimbalTarget.ts', import.meta.url)), 'utf8');
const { code } = esbuild.transformSync(src, { loader: 'ts', format: 'esm' });
const { gimbalTarget, gimbalTipLines, destination, distanceM, formatLatLon, MAX_ESTIMATE_M } = await import(
  'data:text/javascript;base64,' + Buffer.from(code).toString('base64')
);

const near = (a, b, tol) => assert.ok(Math.abs(a - b) <= tol, `${a} ≈ ${b} (±${tol})`);
const fov = (p = {}) => ({
  camLat: -35.36, camLon: 149.16, camAltMsl: 684, poiLat: null, poiLon: null, poiAltMsl: null,
  pitchDeg: -45, yawDeg: 90, hfovDeg: 81, vfovDeg: null, ...p,
});

test('ArduPilot point of interest is used as is', () => {
  const t = gimbalTarget(fov({ poiLat: -35.361, poiLon: 149.161, poiAltMsl: 590 }), 584);
  assert.equal(t.source, 'ardupilot');
  assert.deepEqual([t.lat, t.lon, t.altMsl], [-35.361, 149.161, 590]);
  near(t.groundDistM, distanceM(-35.36, 149.16, -35.361, 149.161), 0.01);
  near(t.slantM, Math.hypot(t.groundDistM, 94), 0.01);
});

test('estimate: 100 m above ground at -45° lands 100 m away along the yaw', () => {
  const t = gimbalTarget(fov({ camAltMsl: 684, pitchDeg: -45, yawDeg: 90 }), 584);
  assert.equal(t.source, 'estimate');
  near(t.groundDistM, 100, 0.01);
  near(distanceM(-35.36, 149.16, t.lat, t.lon), 100, 0.05);
  assert.ok(t.lon > 149.16 && Math.abs(t.lat - -35.36) < 1e-6, 'due east');
  near(t.slantM, Math.hypot(100, 100), 0.01);
  assert.equal(t.altMsl, 584);
});

test('estimate: straight down is right below the camera', () => {
  const t = gimbalTarget(fov({ pitchDeg: -90, yawDeg: 0 }), 584);
  near(t.groundDistM, 0, 1e-6);
  near(t.slantM, 100, 1e-6);
});

test('no estimate at or above the horizon, too far, or with the camera below ground', () => {
  assert.equal(gimbalTarget(fov({ pitchDeg: 0 }), 584), null);
  assert.equal(gimbalTarget(fov({ pitchDeg: -1 }), 584), null, 'within MIN_DOWN_PITCH_DEG of the horizon');
  assert.equal(gimbalTarget(fov({ pitchDeg: 10 }), 584), null);
  assert.equal(gimbalTarget(fov({ pitchDeg: -3, camAltMsl: 684 }), 584) === null, 100 / Math.tan(3 * Math.PI / 180) > MAX_ESTIMATE_M);
  assert.equal(gimbalTarget(fov({ camAltMsl: 500 }), 584), null);
});

test('nothing without a camera position, and no estimate without a ground altitude', () => {
  assert.equal(gimbalTarget(fov({ camLat: null }), 584), null);
  assert.equal(gimbalTarget(fov(), null), null);
  assert.equal(gimbalTarget(fov({ pitchDeg: null }), 584), null);
});

test('destination and distance agree, and longitudes wrap', () => {
  const [lat, lon] = destination(10, 20, 37, 2500);
  near(distanceM(10, 20, lat, lon), 2500, 0.01);
  const [, wrapped] = destination(0, 179.999, 90, 1000);
  assert.ok(wrapped < -179.99, `wrapped to ${wrapped}`);
});

test('formatLatLon gives "lat, lon" at 7 decimals, signs kept', () => {
  assert.equal(formatLatLon(-35.3632641, 149.1653727), '-35.3632641, 149.1653727');
  assert.equal(formatLatLon(1.5, -0.25), '1.5000000, -0.2500000');
});

test('gimbalTipLines: who/distance/source, then lat, lon + altitude, then the copy hint', () => {
  const tr = (k, o) => `${k}${o ? JSON.stringify(o.values) : ''}`;
  const t = gimbalTarget(fov({ camAltMsl: 684, pitchDeg: -45, yawDeg: 90 }), 584);
  const [head, pos, hint] = gimbalTipLines(tr, 'Copter 2', t, false);
  assert.match(head, /gimbal\.target\{"name":"Copter 2"\}/);
  assert.match(head, /gimbal\.dist\{"d":100,"s":141\}/);
  assert.match(head, /gimbal\.sourceEstimate/);
  assert.equal(pos, `${formatLatLon(t.lat, t.lon)} · gimbal.altMsl{"a":584}`);
  assert.equal(hint, 'gimbal.clickToCopy');
  assert.equal(gimbalTipLines(tr, 'x', t, true)[2], 'gimbal.copied');
  assert.equal(gimbalTipLines(tr, 'x', { ...t, altMsl: null, slantM: null }, false)[1], formatLatLon(t.lat, t.lon));
});
