#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Mesh-radio emulator: measures what a fleet of MAVLink drones costs a shared IP mesh (Microhard pDDL
and similar), between the drones and Kite. No third-party packages.

Topology it reproduces: every drone's companion pushes MAVLink over UDP to the GCS, each from its own
address. The drones (e.g. `ardupilot-sitl-manager.py --layout udp`) push to this emulator's listen port;
for every drone the emulator opens a dedicated socket towards Kite, so Kite still sees one peer per drone,
exactly as on the real mesh.

What it measures, per drone and for the whole mesh:
  - downlink / uplink bytes per second (the shared airtime the fleet uses),
  - uplink frames that reached a drone they were not addressed to (target_system names another drone):
    traffic a GCS that copies every frame to every peer wastes on the mesh,
  - group commands: when each drone received the same COMMAND_LONG / COMMAND_INT / SET_MODE, and the
    spread from the first drone to the last.

What it can impair (both directions, one shared medium): datagram loss, latency + jitter, and a total
airtime budget in bytes/s with a bounded queue (a full queue drops, like a radio buffer).

  python tools/simulators/mesh_emulator.py --listen 14560 --gcs 127.0.0.1:14550
  python tools/simulators/mesh_emulator.py --loss 0.05 --latency 80 --jitter 40 --rate 40000
  python tools/simulators/mesh_emulator.py --seconds 120 --json-out /tmp/mesh.json
  python tools/simulators/mesh_emulator.py --camera-uri 'rtsp://127.0.0.1:8554/drone{sysid}/{stream}'

--camera-uri also plays each drone's camera manager (component 100): it answers the GCS's request for
VIDEO_STREAM_INFORMATION with a `main` and a `preview` stream, on the drone's own link — like a companion
whose camera manager speaks through the same router as the flight controller.

RFD multipoint mode (--serial-pty PATH): an RFD900-class radio network instead of an IP mesh. Every drone's
traffic reaches the GCS mixed on ONE serial port (a pseudo-terminal, linked at PATH — in a dev build of
Kite list it with KITE_EXTRA_SERIAL_PORTS=PATH and connect as Serial); what Kite sends is broadcast to
every drone and costs airtime once. The shared, half-duplex airtime is the --rate budget. Approximate
presets (usable payload after the radio's overhead, not vendor-exact):

  64 kbps air rate    --rate 6000  --queue 2048 --latency 30 --jitter 20 --loss 0.01
  125 kbps            --rate 12000 --queue 4096 --latency 25 --jitter 15 --loss 0.01
  250 kbps            --rate 24000 --queue 8192 --latency 20 --jitter 10 --loss 0.01

  python tools/simulators/mesh_emulator.py --serial-pty /tmp/kite-rfd0 --rate 6000 --queue 2048 ...

Point the drones at the listen port (SITL manager: UDP layout, port 14560) and Kite at UDP 14550.
"""

import argparse
import heapq
import json
import os
import random
import select
import socket
import struct
import sys
import time
import tty
from collections import namedtuple

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fc_sim import Dialect, crc16_mcrf4xx, find_dialect  # noqa: E402  (shared MAVLink dialect loader)

GCS_SYSID = 255
COMMAND_MSGS = ('COMMAND_LONG', 'COMMAND_INT', 'SET_MODE')
# Fields that address a message to one system. MANUAL_CONTROL calls it `target`.
TARGET_FIELDS = ('target_system', 'target')

Frame = namedtuple('Frame', 'sysid compid msgid payload size')


# ── Framing ──────────────────────────────────────────────────────────────────

def split_frames(data):
    """Split one UDP datagram into MAVLink frames (v1 and v2, signed v2 included).

    Returns (frames, junk_bytes). A datagram carries whole frames on every real link (ArduPilot,
    mavlink-router, MAVProxy), so a frame cut off at the end of the datagram is counted as junk.
    """
    frames, junk, i, n = [], 0, 0, len(data)
    while i < n:
        magic = data[i]
        if magic == 0xFD:
            hdr = 10
        elif magic == 0xFE:
            hdr = 6
        else:
            junk += 1
            i += 1
            continue
        if i + hdr > n:
            junk += n - i
            break
        plen = data[i + 1]
        total = hdr + plen + 2
        if magic == 0xFD:
            if data[i + 2] & 0x01:
                total += 13                      # signed frame: signature trailer
            sysid, compid = data[i + 5], data[i + 6]
            msgid = int.from_bytes(data[i + 7:i + 10], 'little')
        else:
            sysid, compid, msgid = data[i + 3], data[i + 4], data[i + 5]
        if i + total > n:
            junk += n - i
            break
        frames.append(Frame(sysid, compid, msgid, bytes(data[i + hdr:i + hdr + plen]), total))
        i += total
    return frames, junk


def split_stream(buf):
    """Cut the complete MAVLink frames off the front of a serial byte stream (frames straddle reads there).
    Returns (raw frames, the unfinished rest to keep, skipped junk bytes)."""
    out, junk, i, n = [], 0, 0, len(buf)
    while i < n:
        magic = buf[i]
        if magic == 0xFD:
            hdr = 10
        elif magic == 0xFE:
            hdr = 6
        else:
            junk += 1
            i += 1
            continue
        if i + hdr > n:
            break
        total = hdr + buf[i + 1] + 2
        if magic == 0xFD and buf[i + 2] & 0x01:
            total += 13                          # signed frame: signature trailer
        if i + total > n:
            break
        out.append(bytes(buf[i:i + total]))
        i += total
    return out, bytes(buf[i:]), junk


class Decoder:
    """Message names and target systems, from the dialect Kite links."""

    def __init__(self, dialect):
        self.d = dialect
        self.names = {mid: name for name, (mid, *_rest) in dialect.messages.items()}
        self.targeted = {
            mid for name, (mid, base, ext, _crc) in dialect.messages.items()
            if any(f.name in TARGET_FIELDS for f in base + ext)
        }

    def name(self, msgid):
        return self.names.get(msgid)

    def target(self, frame):
        """The system a frame is addressed to: an int (0 = broadcast), or None if it has no target."""
        if frame.msgid not in self.targeted:
            return None
        fields = self.d.unpack(self.names[frame.msgid], frame.payload)
        for key in TARGET_FIELDS:
            if key in fields:
                return fields[key]
        return None

    def command_key(self, frame):
        """A key identifying 'the same command' across drones, or None for non-command frames."""
        name = self.names.get(frame.msgid)
        if name not in COMMAND_MSGS:
            return None
        f = self.d.unpack(name, frame.payload)
        if name == 'SET_MODE':
            return f'SET_MODE {f["custom_mode"]}'
        if f['command'] == 176:                  # MAV_CMD_DO_SET_MODE: the mode is the command
            return f'{name} DO_SET_MODE {int(f["param2"])}'
        return f'{name} {f["command"]}'


def build_frame(dialect, name, sysid, compid, seq, values):
    """One MAVLink v2 frame of message `name` (no v2 truncation, which is optional for a sender)."""
    msgid, payload = dialect.pack(name, values)
    header = struct.pack('<BBBBBB', len(payload), 0, 0, seq & 0xFF, sysid, compid) + struct.pack('<I', msgid)[:3]
    crc = crc16_mcrf4xx(header + payload)
    crc = crc16_mcrf4xx(bytes([dialect.messages[name][3]]), crc)
    return b'\xfd' + header + payload + struct.pack('<H', crc)


CAMERA_COMPID = 100
MSG_VIDEO_STREAM_INFORMATION = 269
CMD_REQUEST_MESSAGE = 512
# (stream_id, name, width, height, fps, bitrate, encoding: 1 = H.264, 0 = unknown/MJPEG)
CAMERA_STREAMS = [(1, 'main', 1280, 720, 30.0, 2_000_000, 1), (2, 'preview', 320, 176, 5.0, 300_000, 0)]


def camera_answer(decoder, frame, sysid, uri_template, seq=0):
    """Frames a drone's camera answers to one uplink frame: VIDEO_STREAM_INFORMATION per stream for a
    REQUEST_MESSAGE(269) addressed to this system (any component), else nothing."""
    if decoder.name(frame.msgid) != 'COMMAND_LONG':
        return []
    f = decoder.d.unpack('COMMAND_LONG', frame.payload)
    if f['command'] != CMD_REQUEST_MESSAGE or int(f['param1']) != MSG_VIDEO_STREAM_INFORMATION:
        return []
    if f['target_system'] not in (0, sysid) or f['target_component'] not in (0, CAMERA_COMPID):
        return []
    out = []
    for i, (sid, name, w, h, fps, bps, enc) in enumerate(CAMERA_STREAMS):
        uri = uri_template.replace('{sysid}', str(sysid)).replace('{stream}', name)
        out.append(build_frame(decoder.d, 'VIDEO_STREAM_INFORMATION', sysid, CAMERA_COMPID, seq + i, {
            'stream_id': sid, 'count': len(CAMERA_STREAMS), 'type': 0, 'flags': 1, 'framerate': fps,
            'resolution_h': w, 'resolution_v': h, 'bitrate': bps, 'name': name, 'uri': uri, 'encoding': enc,
        }))
    return out


# ── Shared medium ────────────────────────────────────────────────────────────

class Mesh:
    """One shared radio medium: loss, latency + jitter, and an airtime budget with a bounded queue.

    `admit` returns the delivery time of a datagram, or None when it is lost or the queue is full.
    The clock and the random source are passed in so the model is deterministic under test.
    """

    def __init__(self, loss=0.0, latency_s=0.0, jitter_s=0.0, rate_bps=0, queue_bytes=65536, rng=None):
        self.loss, self.latency, self.jitter = loss, latency_s, jitter_s
        self.rate, self.queue_bytes = rate_bps, queue_bytes
        self.rng = rng or random.Random()
        self.free_at = 0.0

    def admit(self, nbytes, now):
        if self.loss and self.rng.random() < self.loss:
            return None
        sent = now
        if self.rate:
            start = max(self.free_at, now)
            if (start - now) * self.rate > self.queue_bytes:
                return None                      # queue full: the radio drops it
            self.free_at = start + nbytes / self.rate
            sent = self.free_at
        return sent + self.latency + (self.rng.uniform(0, self.jitter) if self.jitter else 0.0)


# ── Statistics ───────────────────────────────────────────────────────────────

class DroneStats:
    FIELDS = ('down_bytes', 'up_bytes', 'down_frames', 'up_frames', 'up_wasted', 'down_dropped', 'up_dropped')

    def __init__(self, src):
        self.src, self.sysid = src, None
        for k in self.FIELDS:
            setattr(self, k, 0)

    def snapshot(self):
        return {k: getattr(self, k) for k in self.FIELDS}


def count_uplink(stats, frames, decoder):
    """Account uplink frames delivered to one drone; a frame addressed to another drone is waste."""
    for fr in frames:
        stats.up_frames += 1
        target = decoder.target(fr)
        if target and stats.sysid is not None and target != stats.sysid:
            stats.up_wasted += 1


def command_bursts(events, window_s=30.0):
    """Group per-drone command deliveries into bursts: the same command reaching several drones.

    `events` is a list of (time, key, sysid). Returns [{key, drones, spread_s, first_t}] for every burst
    that reached two or more distinct drones; a burst ends when the same key goes quiet for `window_s`.
    """
    by_key = {}
    for t, key, sysid in sorted(events):
        by_key.setdefault(key, []).append((t, sysid))
    out = []
    for key, evs in by_key.items():
        burst = []
        for t, sysid in evs + [(float('inf'), None)]:
            if burst and t - burst[-1][0] > window_s:
                firsts = {}
                for bt, bs in burst:
                    firsts.setdefault(bs, bt)
                if len(firsts) >= 2:
                    times = sorted(firsts.values())
                    out.append({'key': key, 'drones': len(firsts), 'spread_s': times[-1] - times[0],
                                'first_t': times[0]})
                burst = []
            if sysid is not None:
                burst.append((t, sysid))
    return sorted(out, key=lambda b: b['first_t'])


# ── Relay ────────────────────────────────────────────────────────────────────

class Emulator:
    def __init__(self, args, decoder):
        self.args, self.dec = args, decoder
        host, _, port = args.gcs.rpartition(':')
        self.gcs = (host or '127.0.0.1', int(port))
        self.down = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.down.bind(('0.0.0.0', args.listen))
        self.down.setblocking(False)
        self.mesh = Mesh(args.loss, args.latency / 1000, args.jitter / 1000, args.rate, args.queue)
        self.drones = {}        # drone source addr -> (up socket, DroneStats)
        self.by_sock = {}       # up socket -> drone source addr
        self.pending = []       # heap of (deliver_at, seq, socket, data, addr)
        self.seq = 0
        self.commands = []      # (t, key, sysid) at delivery to the addressed drone
        self.camera_seq = 0
        self.t0 = time.monotonic()
        # Seconds each admitted datagram waits for airtime (+ latency) in the current report window.
        self.delays = []
        # RFD multipoint mode: the GCS side is one serial port (pty master), not a socket per drone.
        self.serial_fd = None
        self.serial_rx = b''
        self.air_up_bytes = 0       # uplink airtime (broadcast: counted once, not per drone)
        self.serial_overflow = 0    # parts dropped because nobody reads the serial port
        if args.serial_pty:
            self.serial_fd, slave = os.openpty()
            tty.setraw(slave)
            os.set_blocking(self.serial_fd, False)
            self._slave = slave     # kept open: the port survives Kite disconnecting / reconnecting
            if os.path.lexists(args.serial_pty):
                os.remove(args.serial_pty)
            os.symlink(os.ttyname(slave), args.serial_pty)
            print(f'[mesh] RFD multipoint: GCS serial port {args.serial_pty} -> {os.ttyname(slave)}')

    def _drone(self, src):
        if src not in self.drones:
            if self.serial_fd is not None:
                self.drones[src] = (None, DroneStats(src))
                print(f'[mesh] drone at {src[0]}:{src[1]} -> serial ({len(self.drones)} drone(s))')
                return self.drones[src]
            up = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            up.bind(('0.0.0.0', 0))
            up.setblocking(False)
            self.drones[src] = (up, DroneStats(src))
            self.by_sock[up] = src
            print(f'[mesh] drone at {src[0]}:{src[1]} -> Kite {self.gcs[0]}:{self.gcs[1]} '
                  f'from :{up.getsockname()[1]} ({len(self.drones)} drone(s))')
        return self.drones[src]

    def _admit(self, nbytes, now):
        at = self.mesh.admit(nbytes, now)
        if at is not None:
            self.delays.append(at - now)
        return at

    def _schedule(self, at, sock, data, addr):
        heapq.heappush(self.pending, (at, self.seq, sock, data, addr))
        self.seq += 1

    def on_downlink(self, data, src, now):
        up, st = self._drone(src)
        frames, _junk = split_frames(data)
        if st.sysid is None:
            st.sysid = next((f.sysid for f in frames if f.sysid != GCS_SYSID), None)
        at = self._admit(len(data), now)
        if at is None:
            st.down_dropped += 1
            return
        st.down_bytes += len(data)
        st.down_frames += len(frames)
        self._schedule(at, up, data, self.gcs)   # up is None in serial mode → the serial port

    def on_serial(self, chunk, now):
        """RFD multipoint uplink: Kite's serial bytes → frames, each broadcast to every drone (airtime once)."""
        frames, self.serial_rx, _junk = split_stream(self.serial_rx + chunk)
        for raw in frames:
            at = self._admit(len(raw), now)
            if at is None:
                for _up, st in self.drones.values():
                    st.up_dropped += 1
                continue
            self.air_up_bytes += len(raw)
            (fr,), _ = split_frames(raw)
            key, target = self.dec.command_key(fr), self.dec.target(fr)
            for src, (_up, st) in self.drones.items():
                st.up_frames += 1
                if key and st.sysid is not None and target in (0, st.sysid):
                    self.commands.append((at - self.t0, key, st.sysid))
                if self.args.camera_uri and st.sysid is not None:
                    for reply in camera_answer(self.dec, fr, st.sysid, self.args.camera_uri, self.camera_seq):
                        self.camera_seq += 1
                        back = self._admit(len(reply), at)
                        if back is not None:
                            self._schedule(back, None, reply, self.gcs)
                self._schedule(at, self.down, raw, src)

    def on_uplink(self, data, up, now):
        src = self.by_sock[up]
        _sock, st = self.drones[src]
        at = self._admit(len(data), now)
        if at is None:
            st.up_dropped += 1
            return
        st.up_bytes += len(data)
        frames, _junk = split_frames(data)
        count_uplink(st, frames, self.dec)
        for fr in frames:
            key = self.dec.command_key(fr)
            target = self.dec.target(fr)
            if key and st.sysid is not None and target in (0, st.sysid):
                self.commands.append((at - self.t0, key, st.sysid))
            if self.args.camera_uri and st.sysid is not None:
                # The drone's camera answers over the drone's own link (after the request crossed the mesh).
                for reply in camera_answer(self.dec, fr, st.sysid, self.args.camera_uri, self.camera_seq):
                    self.camera_seq += 1
                    back = self._admit(len(reply), at)
                    if back is not None:
                        self._schedule(back, up, reply, self.gcs)
        self._schedule(at, self.down, data, src)

    def flush(self, now):
        while self.pending and self.pending[0][0] <= now:
            _at, _seq, sock, data, addr = heapq.heappop(self.pending)
            try:
                if sock is None:
                    os.write(self.serial_fd, data)   # RFD mode: onto the GCS serial port
                else:
                    sock.sendto(data, addr)
            except BlockingIOError:
                self.serial_overflow += 1        # nobody reads the serial port — the part is lost
            except OSError:
                pass                             # peer gone; the datagram is lost like on air

    def run(self):
        a = self.args
        last_report, prev = time.monotonic(), {}
        end = self.t0 + a.seconds if a.seconds else None
        try:
            while end is None or time.monotonic() < end:
                now = time.monotonic()
                timeout = 0.05
                if self.pending:
                    timeout = max(0.0, min(timeout, self.pending[0][0] - now))
                watch = [self.down, *self.by_sock] + ([self.serial_fd] if self.serial_fd is not None else [])
                readable, _w, _x = select.select(watch, [], [], timeout)
                now = time.monotonic()
                for sock in readable:
                    if sock == self.serial_fd:
                        while True:
                            try:
                                chunk = os.read(self.serial_fd, 4096)
                            except OSError:
                                break
                            if not chunk:
                                break
                            self.on_serial(chunk, now)
                        continue
                    while True:
                        try:
                            data, addr = sock.recvfrom(65535)
                        except (BlockingIOError, ConnectionResetError):
                            break
                        if sock is self.down:
                            self.on_downlink(data, addr, now)
                        else:
                            self.on_uplink(data, sock, now)
                self.flush(time.monotonic())
                if now - last_report >= a.report:
                    prev = self.report(now - last_report, prev)
                    last_report = now
        except KeyboardInterrupt:
            pass
        finally:
            if self.args.serial_pty and os.path.islink(self.args.serial_pty):
                os.remove(self.args.serial_pty)
        return self.summary()

    def report(self, dt, prev):
        rows, tot_down, tot_up = [], 0, 0
        if self.serial_fd is not None:
            tot_up = self.air_up_bytes - prev.get('_air_up', 0)
        cur = {}
        for _up, st in sorted(self.drones.values(), key=lambda v: v[1].sysid or 0):
            snap = st.snapshot()
            cur[st.src] = snap
            p = prev.get(st.src, dict.fromkeys(DroneStats.FIELDS, 0))
            d = {k: snap[k] - p[k] for k in snap}
            tot_down += d['down_bytes']
            if self.serial_fd is None:
                tot_up += d['up_bytes']
            wasted = 100.0 * d['up_wasted'] / d['up_frames'] if d['up_frames'] else 0.0
            rows.append(f"  {st.sysid if st.sysid is not None else '?':>5}  {d['down_bytes'] / dt:>9.0f}  "
                        f"{d['up_bytes'] / dt:>9.0f}  {d['up_frames'] / dt:>8.1f}  {wasted:>6.1f}%  "
                        f"{d['down_dropped'] + d['up_dropped']:>5}")
        print(f"\n[mesh] t={time.monotonic() - self.t0:6.0f}s  drones={len(self.drones)}  "
              f"down={tot_down / dt:.0f} B/s  up={tot_up / dt:.0f} B/s  total={(tot_down + tot_up) / dt:.0f} B/s"
              + (f"  ({100.0 * (tot_down + tot_up) / dt / self.args.rate:.0f}% of budget)" if self.args.rate else ''))
        if self.delays:
            ds = sorted(self.delays)
            print(f"  airtime wait: avg {1000 * sum(ds) / len(ds):.0f} ms, p95 {1000 * ds[int(0.95 * (len(ds) - 1))]:.0f} ms, "
                  f"max {1000 * ds[-1]:.0f} ms" + (f"  serial overflow {self.serial_overflow}" if self.serial_fd is not None else ''))
            self.delays = []
        print('  sysid   down B/s    up B/s   up fr/s  wasted  drops')
        print('\n'.join(rows))
        cur['_air_up'] = self.air_up_bytes
        for b in command_bursts(self.commands)[-3:]:
            print(f"  group cmd {b['key']}: {b['drones']} drones, first->last {b['spread_s'] * 1000:.0f} ms")
        return cur

    def summary(self):
        elapsed = max(1e-9, time.monotonic() - self.t0)
        drones = []
        for _up, st in self.drones.values():
            s = st.snapshot()
            s.update(sysid=st.sysid, src=f'{st.src[0]}:{st.src[1]}',
                     down_bps=s['down_bytes'] / elapsed, up_bps=s['up_bytes'] / elapsed)
            drones.append(s)
        return {
            'elapsed_s': elapsed,
            'drones': sorted(drones, key=lambda d: d['sysid'] or 0),
            'total_down_bps': sum(d['down_bps'] for d in drones),
            'total_up_bps': (self.air_up_bytes / elapsed) if self.serial_fd is not None
                            else sum(d['up_bps'] for d in drones),
            'serial_overflow': self.serial_overflow,
            'up_frames': sum(d['up_frames'] for d in drones),
            'up_wasted': sum(d['up_wasted'] for d in drones),
            'group_commands': command_bursts(self.commands),
            'impairment': {'loss': self.args.loss, 'latency_ms': self.args.latency,
                           'jitter_ms': self.args.jitter, 'rate_bps': self.args.rate},
        }


def main():
    p = argparse.ArgumentParser(description='Mesh-radio emulator between MAVLink drones and Kite')
    p.add_argument('--listen', type=int, default=14560, help='UDP port the drones push to (default 14560)')
    p.add_argument('--gcs', default='127.0.0.1:14550', help="Kite's UDP host:port (default 127.0.0.1:14550)")
    p.add_argument('--loss', type=float, default=0.0, help='datagram loss probability, 0..1')
    p.add_argument('--latency', type=float, default=0.0, help='one-way latency in ms')
    p.add_argument('--jitter', type=float, default=0.0, help='extra random latency 0..N ms')
    p.add_argument('--rate', type=int, default=0, help='shared airtime budget in bytes/s (0 = unlimited)')
    p.add_argument('--queue', type=int, default=65536, help='radio queue in bytes before drops')
    p.add_argument('--report', type=float, default=5.0, help='seconds between status tables')
    p.add_argument('--seconds', type=float, default=0.0, help='stop after this many seconds (0 = Ctrl+C)')
    p.add_argument('--json-out', help='write the final summary to this JSON file')
    p.add_argument('--defs', help='MAVLink dialect XML (default: the one in the mavlink crate)')
    p.add_argument('--serial-pty', help='RFD multipoint mode: serve the GCS side as ONE serial port (a pty '
                                        'linked at this path) instead of a UDP socket per drone')
    p.add_argument('--camera-uri', help="answer VIDEO_STREAM_INFORMATION requests as each drone's camera, "
                                        "URIs from this template ({sysid}, {stream} = main|preview)")
    a = p.parse_args()
    defs = a.defs or find_dialect()
    if not defs:
        sys.exit('MAVLink dialect XML not found — run `cargo check` in src-tauri once, or pass --defs')
    summary = Emulator(a, Decoder(Dialect(defs))).run()
    print(f"\n[mesh] done: {len(summary['drones'])} drones, down {summary['total_down_bps']:.0f} B/s, "
          f"up {summary['total_up_bps']:.0f} B/s, wasted uplink frames "
          f"{summary['up_wasted']}/{summary['up_frames']}")
    if a.json_out:
        with open(a.json_out, 'w', encoding='utf-8') as f:
            json.dump(summary, f, indent=2)
        print(f'[mesh] summary written to {a.json_out}')


if __name__ == '__main__':
    main()
