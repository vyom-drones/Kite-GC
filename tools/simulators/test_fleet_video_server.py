#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Unit tests for fleet_video_server.py (no GStreamer needed).
Run: python3 -m unittest tools/simulators/test_fleet_video_server.py"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fleet_video_server import PATTERNS, PROFILES, launch_string, mount_path  # noqa: E402


class FleetVideoServerTest(unittest.TestCase):
    def test_mount_paths_match_the_kite_template(self):
        # Kite: rtsp://127.0.0.1:8554/drone{sysid}/{stream}
        self.assertEqual(mount_path(7, 'main'), '/drone7/main')
        self.assertEqual(mount_path(12, 'preview'), '/drone12/preview')

    def test_main_and_preview_profiles(self):
        main = launch_string(3, 'main')
        self.assertIn('width=1280,height=720,framerate=30/1', main)
        self.assertIn('bitrate=2000', main)
        preview = launch_string(3, 'preview')
        self.assertIn('width=320,height=176,framerate=5/1', preview)
        self.assertIn('bitrate=150', preview)

    def test_every_picture_names_its_drone_and_stream(self):
        self.assertIn('text="DRONE 3 · MAIN"', launch_string(3, 'main'))
        self.assertIn('text="DRONE 20 · PREVIEW"', launch_string(20, 'preview'))

    def test_keyframe_every_second_so_a_new_viewer_sees_a_picture_fast(self):
        self.assertIn('key-int-max=30', launch_string(1, 'main'))
        self.assertIn('key-int-max=5', launch_string(1, 'preview'))

    def test_neighbouring_drones_get_different_patterns(self):
        pats = [launch_string(d, 'main').split('pattern=')[1].split()[0] for d in range(1, len(PATTERNS) + 1)]
        self.assertEqual(len(set(pats)), len(PATTERNS))

    def test_main_bitrate_override_leaves_the_preview_alone(self):
        self.assertIn('bitrate=800', launch_string(1, 'main', main_kbps=800))
        self.assertIn('bitrate=150', launch_string(1, 'preview', main_kbps=800))

    def test_unknown_stream_is_rejected(self):
        with self.assertRaises(ValueError):
            launch_string(1, 'thermal')

    def test_mjpeg_preview_option(self):
        p = launch_string(4, 'preview', preview_codec='mjpeg')
        self.assertIn('jpegenc', p)
        self.assertIn('rtpjpegpay name=pay0', p)
        self.assertNotIn('x264enc', p)
        self.assertIn('x264enc', launch_string(4, 'main', preview_codec='mjpeg'), 'main stays H.264')
        # RFC 2435 cannot carry 4:4:4 — the JPEG must be 4:2:0 (else the mount never answers DESCRIBE).
        self.assertIn('format=I420 ! jpegenc', p)
        with self.assertRaises(ValueError):
            launch_string(4, 'preview', preview_codec='vp8')

    def test_preview_size_fits_rtp_jpeg(self):
        w, h = PROFILES['preview'][:2]
        self.assertEqual((w % 8, h % 8), (0, 0), 'RFC 2435 sizes are multiples of 8')

    def test_pipeline_ends_in_the_payloader_the_server_needs(self):
        self.assertIn('rtph264pay name=pay0', launch_string(1, 'main'))


if __name__ == '__main__':
    unittest.main()
