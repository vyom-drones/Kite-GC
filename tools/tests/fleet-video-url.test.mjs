// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom
//
// Unit tests for src/lib/helpers/fleetVideoUrl.ts (transpiled with Vite's esbuild).
//   node --test tools/tests/

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import esbuild from 'esbuild';

const src = readFileSync(fileURLToPath(new URL('../../src/lib/helpers/fleetVideoUrl.ts', import.meta.url)), 'utf8');
const { code } = esbuild.transformSync(src, { loader: 'ts', format: 'esm' });
const { resolveStreamUrl, fillTemplate, overrideKey, parseFleetVideoConfig, followAction, pickDiscovered, DEFAULT_FLEET_VIDEO } = await import(
  'data:text/javascript;base64,' + Buffer.from(code).toString('base64')
);

const v7 = { vehicleId: 'L1:S7', linkId: 1, sysid: 7, name: 'Scout 7', fcUid: 'ABC123' };
const cfg = (p = {}) => ({ ...DEFAULT_FLEET_VIDEO, overrides: {}, ...p });

test('template placeholders are filled per vehicle and stream', () => {
  const c = cfg({ template: 'rtsp://10.10.0.{sysid}:8554/{stream}' });
  assert.equal(resolveStreamUrl(c, v7, 'main'), 'rtsp://10.10.0.7:8554/main');
  assert.equal(resolveStreamUrl(c, v7, 'preview'), 'rtsp://10.10.0.7:8554/preview');
});

test('every placeholder, names URL-encoded, unknown braces left alone', () => {
  assert.equal(
    fillTemplate('rtsp://h/{link}/{sysid}/{name}/{fcuid}/{stream}/{other}', v7, 'main'),
    'rtsp://h/1/7/Scout%207/ABC123/main/{other}',
  );
  assert.equal(fillTemplate('x/{fcuid}', { ...v7, fcUid: null }, 'main'), 'x/');
});

test('no template and no override → no URL', () => {
  assert.equal(resolveStreamUrl(cfg(), v7, 'main'), null);
  assert.equal(resolveStreamUrl(cfg({ template: '   ' }), v7, 'main'), null);
});

test('an override wins over the template', () => {
  const c = cfg({ template: 'rtsp://10.10.0.{sysid}/{stream}', overrides: { 'fc:ABC123': 'rtsp://cam7.local/live' } });
  assert.equal(resolveStreamUrl(c, v7, 'main'), 'rtsp://cam7.local/live');
});

test('an override without {stream} has no preview; with {stream} it has both', () => {
  const plain = cfg({ overrides: { 'fc:ABC123': 'rtsp://cam7.local/live' } });
  assert.equal(resolveStreamUrl(plain, v7, 'preview'), null);
  const both = cfg({ overrides: { 'fc:ABC123': 'rtsp://cam7.local/{stream}' } });
  assert.equal(resolveStreamUrl(both, v7, 'preview'), 'rtsp://cam7.local/preview');
});

test('override key: FC id when known, else the system id', () => {
  assert.equal(overrideKey(v7), 'fc:ABC123');
  assert.equal(overrideKey({ sysid: 9, fcUid: null }), 'sys:9');
  const c = cfg({ overrides: { 'sys:9': 'rtsp://nine' } });
  assert.equal(resolveStreamUrl(c, { ...v7, sysid: 9, fcUid: null }, 'main'), 'rtsp://nine');
});

test('parse keeps defaults for missing or malformed fields and drops empty overrides', () => {
  assert.deepEqual(parseFleetVideoConfig(null), DEFAULT_FLEET_VIDEO);
  assert.deepEqual(parseFleetVideoConfig({ followActive: 'yes', template: 5, overrides: { a: '  ', b: 'rtsp://b', c: 3 } }), {
    followActive: true, template: '', overrides: { b: 'rtsp://b' },
  });
  assert.deepEqual(parseFleetVideoConfig({ followActive: false, template: 't' }), { followActive: false, template: 't', overrides: {} });
});

const fa = (p) => followAction({ kind: 'rtsp', enabled: true, url: 'rtsp://a', paused: false, following: true, ...p });

test('follow: a URL switches the RTSP source, or resumes a feed the follower paused', () => {
  assert.equal(fa({}), 'switch');
  assert.equal(fa({ enabled: false }), 'switch', 'a stopped feed only gets the new URL (followRtspUrl does not start it)');
  assert.equal(fa({ paused: true, enabled: false }), 'resume');
});

test('follow: an active vehicle without a stream pauses a live feed — never shows another aircraft', () => {
  assert.equal(fa({ url: null }), 'pause');
  assert.equal(fa({ url: null, enabled: false }), 'none', 'nothing running → nothing to pause');
  assert.equal(fa({ url: null, following: false }), 'none', 'following off → the operator\'s URL stays');
});

test('follow: a local camera or capture device is never replaced', () => {
  for (const kind of ['camera', 'native']) {
    assert.equal(fa({ kind }), 'none');
    assert.equal(fa({ kind, url: null }), 'none');
  }
});

const ds = (p) => ({ compid: 100, streamId: 1, kind: 'rtsp', name: '', uri: 'rtsp://cam/x', width: 1280, height: 720, ...p });

test('discovered: named main / preview streams are picked by name', () => {
  const streams = [ds({ streamId: 1, name: 'Preview', uri: 'rtsp://c/p', width: 320, height: 176 }), ds({ streamId: 2, name: 'Main', uri: 'rtsp://c/m' })];
  assert.equal(pickDiscovered(streams, 'main'), 'rtsp://c/m');
  assert.equal(pickDiscovered(streams, 'preview'), 'rtsp://c/p');
});

test('discovered: unnamed streams — largest is main, smallest is preview', () => {
  const streams = [ds({ streamId: 1, uri: 'rtsp://c/big', width: 1920, height: 1080 }), ds({ streamId: 2, uri: 'rtsp://c/small', width: 640, height: 360 })];
  assert.equal(pickDiscovered(streams, 'main'), 'rtsp://c/big');
  assert.equal(pickDiscovered(streams, 'preview'), 'rtsp://c/small');
});

test('discovered: a single stream is main only; non-RTSP and empty URIs are ignored', () => {
  assert.equal(pickDiscovered([ds({ uri: 'rtsp://c/only' })], 'main'), 'rtsp://c/only');
  assert.equal(pickDiscovered([ds({ uri: 'rtsp://c/only' })], 'preview'), null);
  assert.equal(pickDiscovered([ds({ kind: 'rtpudp', uri: 'udp://0.0.0.0:5600' })], 'main'), null);
  assert.equal(pickDiscovered([ds({ uri: '' })], 'main'), null);
  assert.equal(pickDiscovered([], 'main'), null);
});

test('discovered streams come last: override, then template, then the camera', () => {
  const found = [ds({ name: 'main', uri: 'rtsp://cam/main' }), ds({ streamId: 2, name: 'preview', uri: 'rtsp://cam/prev', width: 320, height: 176 })];
  assert.equal(resolveStreamUrl(cfg(), v7, 'main', found), 'rtsp://cam/main');
  assert.equal(resolveStreamUrl(cfg(), v7, 'preview', found), 'rtsp://cam/prev');
  assert.equal(resolveStreamUrl(cfg({ template: 'rtsp://t/{stream}' }), v7, 'main', found), 'rtsp://t/main');
  assert.equal(resolveStreamUrl(cfg({ overrides: { 'fc:ABC123': 'rtsp://o' } }), v7, 'main', found), 'rtsp://o');
});
