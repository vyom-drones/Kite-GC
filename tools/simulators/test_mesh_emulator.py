#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Unit tests for mesh_emulator.py.  Run: python -m unittest tools/simulators/test_mesh_emulator.py"""

import os
import random
import struct
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fc_sim import Dialect, crc16_mcrf4xx, find_dialect  # noqa: E402
from mesh_emulator import (  # noqa: E402
    Decoder, DroneStats, Mesh, command_bursts, count_uplink, split_frames,
)

DEFS = find_dialect()


def frame_v2(d, name, sysid=1, compid=1, signed=False, **values):
    msgid, payload = d.pack(name, values)
    payload = payload.rstrip(b'\x00') or b'\x00'          # v2 trailing-zero truncation
    header = struct.pack('<BBBBBB', len(payload), 0x01 if signed else 0, 0, 0, sysid, compid)
    header += struct.pack('<I', msgid)[:3]
    crc = crc16_mcrf4xx(header + payload)
    crc = crc16_mcrf4xx(bytes([d.messages[name][3]]), crc)
    return b'\xfd' + header + payload + struct.pack('<H', crc) + (b'\x00' * 13 if signed else b'')


def frame_v1(msgid, payload, sysid=1, compid=1):
    return bytes([0xFE, len(payload), 0, sysid, compid, msgid]) + payload + b'\x00\x00'


class SplitFramesTest(unittest.TestCase):
    def test_v1_and_v2_in_one_datagram(self):
        data = frame_v1(0, b'\x01' * 9, sysid=7) + bytes([0xFD, 2, 0, 0, 0, 9, 1, 0x4C, 0, 0]) + b'ab' + b'cc'
        frames, junk = split_frames(data)
        self.assertEqual(junk, 0)
        self.assertEqual([(f.sysid, f.msgid) for f in frames], [(7, 0), (9, 76)])
        self.assertEqual(frames[1].payload, b'ab')

    def test_signed_v2_skips_signature(self):
        signed = bytes([0xFD, 1, 0x01, 0, 0, 3, 1, 0, 0, 0]) + b'x' + b'cc' + b'S' * 13
        frames, junk = split_frames(signed + frame_v1(0, b'', sysid=4))
        self.assertEqual(junk, 0)
        self.assertEqual([f.sysid for f in frames], [3, 4])
        self.assertEqual(frames[0].size, 10 + 1 + 2 + 13)

    def test_garbage_and_truncated_frame_are_junk(self):
        whole = frame_v1(0, b'\x00' * 9, sysid=2)
        frames, junk = split_frames(b'\x00\x11' + whole + whole[:5])
        self.assertEqual(len(frames), 1)
        self.assertEqual(junk, 2 + 5)

    def test_empty(self):
        self.assertEqual(split_frames(b''), ([], 0))


@unittest.skipUnless(DEFS, 'MAVLink dialect XML not found (run cargo check in src-tauri once)')
class DecoderTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.d = Dialect(DEFS)
        cls.dec = Decoder(cls.d)

    def one(self, raw):
        frames, junk = split_frames(raw)
        self.assertEqual((len(frames), junk), (1, 0))
        return frames[0]

    def test_command_long_target_and_key(self):
        fr = self.one(frame_v2(self.d, 'COMMAND_LONG', sysid=255, target_system=12, target_component=1,
                               command=20))
        self.assertEqual(self.dec.target(fr), 12)
        self.assertEqual(self.dec.command_key(fr), 'COMMAND_LONG 20')

    def test_do_set_mode_key_includes_mode(self):
        fr = self.one(frame_v2(self.d, 'COMMAND_LONG', sysid=255, target_system=3, command=176,
                               param1=1.0, param2=6.0))
        self.assertEqual(self.dec.command_key(fr), 'COMMAND_LONG DO_SET_MODE 6')

    def test_heartbeat_has_no_target(self):
        fr = self.one(frame_v2(self.d, 'HEARTBEAT', sysid=255, type=6, autopilot=8))
        self.assertIsNone(self.dec.target(fr))
        self.assertIsNone(self.dec.command_key(fr))

    def test_truncated_broadcast_target_is_zero(self):
        # target_system 0 at the end of a v2 payload is trimmed away; it must decode as broadcast.
        fr = self.one(frame_v2(self.d, 'PARAM_REQUEST_LIST', sysid=255, target_system=0, target_component=0))
        self.assertEqual(self.dec.target(fr), 0)

    def test_manual_control_uses_target_field(self):
        fr = self.one(frame_v2(self.d, 'MANUAL_CONTROL', sysid=255, target=5, x=100))
        self.assertEqual(self.dec.target(fr), 5)

    def test_count_uplink_marks_frames_for_other_drones_as_waste(self):
        st = DroneStats(('127.0.0.1', 1))
        st.sysid = 4
        raw = (frame_v2(self.d, 'COMMAND_LONG', sysid=255, target_system=4, command=20)
               + frame_v2(self.d, 'COMMAND_LONG', sysid=255, target_system=9, command=20)
               + frame_v2(self.d, 'HEARTBEAT', sysid=255, type=6, autopilot=8)
               + frame_v2(self.d, 'PARAM_REQUEST_LIST', sysid=255, target_system=0))
        count_uplink(st, split_frames(raw)[0], self.dec)
        self.assertEqual((st.up_frames, st.up_wasted), (4, 1))


class MeshTest(unittest.TestCase):
    def test_passthrough_without_impairment(self):
        self.assertEqual(Mesh().admit(100, now=5.0), 5.0)

    def test_latency_and_jitter_bounds(self):
        m = Mesh(latency_s=0.1, jitter_s=0.05, rng=random.Random(1))
        for _ in range(100):
            t = m.admit(10, now=0.0)
            self.assertTrue(0.1 <= t <= 0.15)

    def test_loss_rate_is_respected(self):
        m = Mesh(loss=0.25, rng=random.Random(42))
        lost = sum(m.admit(10, 0.0) is None for _ in range(10000))
        self.assertTrue(2200 < lost < 2800, lost)

    def test_rate_serializes_airtime(self):
        m = Mesh(rate_bps=1000)
        self.assertAlmostEqual(m.admit(100, 0.0), 0.1)
        self.assertAlmostEqual(m.admit(100, 0.0), 0.2)      # queued behind the first
        self.assertAlmostEqual(m.admit(100, 1.0), 1.1)      # medium idle again

    def test_full_queue_drops(self):
        m = Mesh(rate_bps=1000, queue_bytes=250)
        delivered = [m.admit(100, 0.0) for _ in range(5)]
        # Backlog seen by each arrival: 0, 100, 200 B (accepted), then 300 B > 250 (dropped, twice).
        self.assertEqual([d is not None for d in delivered], [True, True, True, False, False])


class CommandBurstsTest(unittest.TestCase):
    def test_spread_counts_first_delivery_per_drone(self):
        ev = [(10.0, 'RTL', 1), (10.2, 'RTL', 2), (10.2, 'RTL', 1), (12.5, 'RTL', 3)]
        (b,) = command_bursts(ev)
        self.assertEqual((b['key'], b['drones']), ('RTL', 3))
        self.assertAlmostEqual(b['spread_s'], 2.5)

    def test_single_drone_is_not_a_group(self):
        self.assertEqual(command_bursts([(1.0, 'ARM', 1), (2.0, 'ARM', 1)]), [])

    def test_quiet_gap_splits_bursts_and_keys_stay_apart(self):
        ev = [(0.0, 'A', 1), (1.0, 'A', 2), (100.0, 'A', 1), (100.5, 'A', 2), (0.5, 'B', 1), (0.6, 'B', 2)]
        bursts = command_bursts(ev, window_s=30)
        self.assertEqual([(b['key'], round(b['spread_s'], 2)) for b in bursts],
                         [('A', 1.0), ('B', 0.1), ('A', 0.5)])


if __name__ == '__main__':
    unittest.main()
