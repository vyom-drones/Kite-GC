#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Simulated drone cameras for Kite's fleet video: one RTSP server, N labelled streams per drone.

Each drone d (1..N) gets two mounts, like a companion computer serving its camera:
  rtsp://<host>:<port>/drone<d>/main      1280x720 @ 30 fps H.264 (~2 Mbps)
  rtsp://<host>:<port>/drone<d>/preview    320x176 @  5 fps H.264 (~150 kbps), or MJPEG with
                                           --preview-codec mjpeg (~300 kbps, no transcoding in Kite)
With --codec h265 both H.26x streams are H.265/HEVC instead, like a SIYI A8 set to H.265 output.
Every picture carries "DRONE d · MAIN|PREVIEW" plus a running clock and a test pattern of its own, so
the operator sees at a glance WHICH drone is on screen and that it is live.

Encoding runs only while a client watches a mount (the server builds the pipeline on demand and drops
it with the last client) — the same "only watched streams flow" property the real fleet relies on. Every
client connect / disconnect is logged with the number of streams currently being served.

In Kite: Video → RTSP → Fleet video, template  rtsp://127.0.0.1:8554/drone{sysid}/{stream}

  python3 tools/simulators/fleet_video_server.py --count 20
  python3 tools/simulators/fleet_video_server.py --count 5 --port 8554 --main-kbps 1500
  python3 tools/simulators/fleet_video_server.py --count 5 --codec h265

Needs GStreamer with gst-rtsp-server and its Python bindings (Debian/Ubuntu: gir1.2-gst-rtsp-server-1.0,
gstreamer1.0-plugins-ugly for x264enc, gstreamer1.0-plugins-bad for x265enc, gstreamer1.0-plugins-base for
textoverlay).
"""

import argparse
import sys
import time

# videotestsrc patterns that look clearly different from each other.
PATTERNS = ['smpte', 'ball', 'snow', 'circular', 'checkers-8', 'zone-plate', 'gamut', 'chroma-zone-plate',
            'spokes', 'gradient', 'colors', 'pinwheel', 'smpte75', 'bar', 'smpte100', 'blink']

PROFILES = {
    # stream: (width, height, fps, kbps, font size)
    'main': (1280, 720, 30, 2000, 36),
    'preview': (320, 176, 5, 150, 14),
}


def mount_path(drone, stream):
    return f'/drone{drone}/{stream}'


def launch_string(drone, stream, main_kbps=None, preview_codec='h264', codec='h264'):
    """The gst-rtsp-server launch line of one drone's stream (pure: unit-tested)."""
    if stream not in PROFILES:
        raise ValueError(f'unknown stream {stream!r}')
    if preview_codec not in ('h264', 'mjpeg'):
        raise ValueError(f'unknown preview codec {preview_codec!r}')
    if codec not in ('h264', 'h265'):
        raise ValueError(f'unknown codec {codec!r}')
    w, h, fps, kbps, font = PROFILES[stream]
    if stream == 'main' and main_kbps:
        kbps = main_kbps
    pattern = PATTERNS[(drone - 1) % len(PATTERNS)]
    label = f'DRONE {drone} · {stream.upper()}'
    if stream == 'preview' and preview_codec == 'mjpeg':
        # RTP/JPEG (RFC 2435) carries baseline 4:2:0 / 4:2:2 only, sizes in multiples of 8: an unconstrained
        # jpegenc may pick 4:4:4, the payloader then fails silently and DESCRIBE never answers.
        encode = 'videoconvert ! video/x-raw,format=I420 ! jpegenc quality=60 ! rtpjpegpay name=pay0 pt=26'
    elif codec == 'h265':
        # x265 already defaults to no B-frames under tune=zerolatency; I420 keeps it on the Main profile.
        encode = (f'videoconvert ! video/x-raw,format=I420 ! x265enc tune=zerolatency speed-preset=ultrafast '
                  f'bitrate={kbps} key-int-max={fps} ! video/x-h265,profile=main '
                  f'! rtph265pay name=pay0 pt=96 config-interval=1')
    else:
        encode = (f'videoconvert ! x264enc tune=zerolatency speed-preset=ultrafast bitrate={kbps} '
                  f'key-int-max={fps} bframes=0 '
                  f'! video/x-h264,profile=baseline ! rtph264pay name=pay0 pt=96 config-interval=1')
    return (
        f'( videotestsrc is-live=true pattern={pattern} '
        f'! video/x-raw,width={w},height={h},framerate={fps}/1 '
        f'! textoverlay text="{label}" valignment=top halignment=left font-desc="Sans Bold {font}" '
        f'shaded-background=true '
        f'! clockoverlay valignment=bottom halignment=right time-format="%H:%M:%S" font-desc="Sans {font}" '
        f'shaded-background=true '
        f'! {encode} )'
    )


def main():
    ap = argparse.ArgumentParser(description='Simulated drone cameras (RTSP) for Kite fleet video')
    ap.add_argument('--count', type=int, default=20, help='number of drones (default 20)')
    ap.add_argument('--port', type=int, default=8554, help='RTSP port (default 8554)')
    ap.add_argument('--first', type=int, default=1, help='first drone number / sysid (default 1)')
    ap.add_argument('--main-kbps', type=int, default=None, help='main stream bitrate (default 2000)')
    ap.add_argument('--preview-codec', choices=['h264', 'mjpeg'], default='h264',
                    help='preview stream codec (default h264; mjpeg needs no transcoding in Kite)')
    ap.add_argument('--codec', choices=['h264', 'h265'], default='h264',
                    help='codec of the H.26x streams (default h264; h265 = HEVC, as a SIYI A8 can send)')
    a = ap.parse_args()

    try:
        import gi
        gi.require_version('Gst', '1.0')
        gi.require_version('GstRtspServer', '1.0')
        from gi.repository import Gst, GstRtspServer, GLib
    except (ImportError, ValueError) as e:
        sys.exit(f'GStreamer RTSP server bindings missing ({e}) — see the module docstring')

    Gst.init(None)
    server = GstRtspServer.RTSPServer()
    server.set_service(str(a.port))
    mounts = server.get_mount_points()
    serving = {'n': 0}

    def on_media(_factory, media, name):
        serving['n'] += 1
        print(f'[{time.strftime("%H:%M:%S")}] + {name} streaming ({serving["n"]} active)', flush=True)

        def on_unprepared(_m):
            serving['n'] -= 1
            print(f'[{time.strftime("%H:%M:%S")}] - {name} stopped   ({serving["n"]} active)', flush=True)

        media.connect('unprepared', on_unprepared)

    for d in range(a.first, a.first + a.count):
        for stream in PROFILES:
            f = GstRtspServer.RTSPMediaFactory()
            f.set_launch(launch_string(d, stream, a.main_kbps, a.preview_codec, a.codec))
            f.set_shared(True)  # several viewers of one stream share one encoder
            f.connect('media-configure', on_media, mount_path(d, stream))
            mounts.add_factory(mount_path(d, stream), f)

    if server.attach(None) == 0:
        sys.exit(f'could not listen on RTSP port {a.port}')
    last = a.first + a.count - 1
    preview = 'mjpeg' if a.preview_codec == 'mjpeg' else a.codec
    print(f'[fleet-video] {a.count} drones on rtsp://0.0.0.0:{a.port}/drone<N>/main|preview (N = {a.first}..{last}), '
          f'main {a.codec}, preview {preview}')
    print(f'[fleet-video] Kite template: rtsp://127.0.0.1:{a.port}/drone{{sysid}}/{{stream}}', flush=True)
    try:
        GLib.MainLoop().run()
    except KeyboardInterrupt:
        pass


if __name__ == '__main__':
    main()
