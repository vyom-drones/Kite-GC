// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Marc Hoffmann (b14ckyy)

// UDP Transport
// Connects to a flight controller via UDP socket (e.g. MAVLink radios, Wi-Fi telemetry).
// Note: UDP is connectionless — we bind locally and send/receive to/from a remote address.
// Implements ByteTransport for protocol-agnostic byte-level I/O.
//
// Peer-learning + fixed-port bind (vs. the naive bind(:0)+connect() client model):
//   Wi-Fi telemetry bridges (mavesp8266 / DroneBridge / typical ESP APs at 192.168.4.1) push
//   telemetry to a *fixed* port (usually 14550) — by unicast to a learned client, by broadcast, or
//   from a source port that differs from the one they listen on. An ephemeral local port + a
//   connect()-restricted peer (the old behaviour) silently drops all of that: we'd never receive a
//   single byte (handshake "read 0 bytes"). Mission Planner's default "UDP" mode is a *listener* on
//   14550 for exactly this reason.
//   So we (a) bind to the same port number the user targeted (fall back to ephemeral only if that
//   port is busy) so datagrams sent to that well-known port reach us, and (b) use recv_from/send_to
//   instead of connect(): we seed the peer with the configured host:port (so the initial GCS
//   HEARTBEAT goes out and wakes ArduPilot/the bridge) and then re-target to whatever source actually
//   sends us data. This covers listen-mode bridges, client-mode SITL, and broadcast setups alike.

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use super::{ByteTransport, TransportError};

/// Read timeout for individual recv calls. Short on purpose — bounds the latency the MAVLink handler
/// loop adds to outgoing commands (it services a queued write only once the current blocking recv
/// returns). See the TCP transport for the full rationale.
const READ_TIMEOUT_MS: u64 = 50;

/// An active UDP transport to a flight controller
pub struct UdpTransport {
    /// Configured target (host:port) — initial send destination and the description label.
    configured: String,
    /// Set when the well-known local port was busy: what we bound instead and why it matters. Surfaced
    /// with a handshake failure so "no HEARTBEAT" points at the other listener, not at the vehicle.
    bind_note: Option<String>,
    /// Configured send target — where the initial GCS HEARTBEAT goes, and the fallback while no peer
    /// has spoken yet.
    peer: SocketAddr,
    /// Every source that has sent us data recently, with its last-seen time (peer learning). Several
    /// vehicles pushing to our listening port each speak from their own socket (multi-vehicle SITL /
    /// several telemetry bridges / a radio mesh). A frame addressed to one MAVLink system goes to its
    /// peer only (`sysid_peer`); everything else — broadcasts, the GCS HEARTBEAT, a system not heard
    /// yet — goes to ALL of them. Peers silent for PEER_TTL age out.
    peers: HashMap<SocketAddr, Instant>,
    /// MAVLink routing: the address each system id last spoke from (learned per datagram from the frame
    /// headers). A frame addressed to one vehicle goes to that vehicle's peer only — on a shared radio
    /// mesh, copying every command / mission item / RC frame to every aircraft costs airtime ×N.
    sysid_peer: HashMap<u8, SocketAddr>,
    socket: UdpSocket,
    /// The first ICMP "port unreachable" of the session is logged at warn (a tester's log must show why
    /// nothing arrives); every further one goes to debug so a dead peer doesn't flood the log.
    peer_unreachable_logged: bool,
}

/// A learned peer that has been silent this long is dropped from the send fan-out.
const PEER_TTL: Duration = Duration::from_secs(10);

impl UdpTransport {
    /// Create a UDP transport targeting `host:port`.
    ///
    /// Binds the local socket to `0.0.0.0:port` so we receive datagrams the FC/bridge sends to that
    /// well-known port; falls back to an ephemeral port if it's already in use. Does not `connect()`
    /// — the peer is learned from incoming traffic (see module docs).
    pub fn connect(host: &str, port: u16) -> Result<Self, String> {
        let addr = format!("{}:{}", host, port);

        // Resolve the configured target so the first send (the GCS HEARTBEAT) has a destination.
        let peer = addr
            .to_socket_addrs()
            .map_err(|e| format!("UDP resolve {} failed: {}", addr, e))?
            .next()
            .ok_or_else(|| format!("UDP resolve {} returned no address", addr))?;

        // Prefer binding to the same port number the user targeted (listener-friendly). If that port
        // is busy (another ground station listening there — Mission Planner on 14550/14560 is the
        // typical case), fall back to a STABLE alternate port derived from the target, and only then
        // to an ephemeral one. Stable matters: relays such as Mission Planner's UDP server remember
        // the client address they stream to, and keep streaming to a dead port for a while after we
        // close it. Coming back on the same port makes a reconnect pick the stream up immediately
        // instead of timing out until the relay forgets us.
        let mut bind_note = None;
        let socket = match UdpSocket::bind(("0.0.0.0", port)) {
            Ok(s) => {
                log::info!("UDP bound to local port {} (listening for {})", port, addr);
                s
            }
            Err(e) => {
                let alt = stable_fallback_port(port);
                let (s, bound) = match UdpSocket::bind(("0.0.0.0", alt)) {
                    Ok(s) => (s, alt.to_string()),
                    Err(_) => (
                        UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("UDP bind failed: {}", e))?,
                        "an ephemeral port".to_string(),
                    ),
                };
                log::warn!(
                    "UDP bind to local port {} failed ({}) — bound {} instead; datagrams pushed to {} reach the other listener, not Kite",
                    port, e, bound, port
                );
                bind_note = Some(format!(
                    "Local UDP port {port} is already in use by another program (another ground station listening there?), so Kite listened on {bound} instead. Vehicles that push telemetry to {port} reach that program, not Kite — close its UDP connection, or point the vehicles (or its MAVLink mirror) at a port Kite can own."
                ));
                s
            }
        };

        socket
            .set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)))
            .map_err(|e| format!("Failed to set read timeout: {}", e))?;

        Ok(Self {
            configured: addr,
            bind_note,
            peer,
            peers: HashMap::new(),
            sysid_peer: HashMap::new(),
            socket,
            peer_unreachable_logged: false,
        })
    }

    /// True for the errors Windows reports on a UDP socket after an ICMP "port unreachable" came back
    /// for an earlier datagram — on the next `recv_from` (`WSAECONNRESET`) or `send_to`. UDP is
    /// connectionless: the peer being gone is a stall (ADR-042), not a transport loss — the link stays
    /// up, the stall watchdog reports it, and the peer may come back (a restarted SITL or bridge).
    fn is_peer_unreachable(e: &std::io::Error) -> bool {
        matches!(
            e.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionAborted
        )
    }

    fn note_peer_unreachable(&mut self, op: &str, peer: SocketAddr, e: &std::io::Error) {
        if !self.peer_unreachable_logged {
            self.peer_unreachable_logged = true;
            log::warn!(
                "UDP peer {} is not reachable ({} — {}); the link stays open, nothing arrives until the peer is back",
                peer, op, e
            );
        } else {
            log::debug!("UDP {}: {} — peer still unreachable, ignoring", op, e);
        }
    }
}

/// Alternate local port when the well-known one is taken: the target port + 10000 (14550 → 24550,
/// 14560 → 24560), kept inside the registered/dynamic range. Deterministic across reconnects.
fn stable_fallback_port(port: u16) -> u16 {
    let alt = port as u32 + 10_000;
    if alt <= u16::MAX as u32 { alt as u16 } else { port.wrapping_sub(10_000).max(1024) }
}

impl ByteTransport for UdpTransport {
    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        match self.socket.recv_from(buf) {
            Ok((n, src)) => {
                // Our own echo: with the default target 127.0.0.1:<our port> the first GCS HEARTBEAT goes
                // to ourselves and comes straight back. Learning that "peer" would loop every later frame
                // back into our own parser for the rest of the session — drop it unseen.
                if src.ip().is_loopback() && Some(src.port()) == self.socket.local_addr().ok().map(|a| a.port()) {
                    return Ok(0);
                }
                // Peer learning: remember every source that talks to us (n == 0 never happens for a
                // real datagram). Sends fan out to all of them — see `peers`.
                if self.peers.insert(src, Instant::now()).is_none() {
                    log::debug!("UDP peer learned: {} ({} peer(s) now)", src, self.peers.len());
                }
                for sysid in mavlink_sysids(&buf[..n]) {
                    match self.sysid_peer.insert(sysid, src) {
                        None => log::debug!("UDP route: sysid {} at {}", sysid, src),
                        Some(old) if old != src => log::info!("UDP route: sysid {} moved {} → {}", sysid, old, src),
                        Some(_) => {}
                    }
                }
                Ok(n)
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                Ok(0)
            }
            Err(ref e) if Self::is_peer_unreachable(e) => {
                // The ICMP error carries no source: with one learned peer that is the one that went away,
                // otherwise the best we can name is the configured target.
                let peer = if self.peers.len() == 1 { self.peers.keys().copied().next() } else { None };
                self.note_peer_unreachable("recv", peer.unwrap_or(self.peer), e);
                Ok(0)
            }
            Err(e) => Err(TransportError::from(e)),
        }
    }

    fn write_bytes(&mut self, data: &[u8]) -> Result<(), TransportError> {
        // Age out silent peers, then fan out to every live one; with none learned yet, the configured
        // target gets it (this is how the first GCS HEARTBEAT wakes a client-mode FC / bridge).
        let now = Instant::now();
        self.peers.retain(|_, seen| now.duration_since(*seen) < PEER_TTL);
        let targets: Vec<SocketAddr> = if self.peers.is_empty() {
            vec![self.peer]
        } else {
            self.peers.keys().copied().collect()
        };
        self.send_datagram(targets, data)
    }

    fn write_bytes_to(&mut self, target: Option<u8>, data: &[u8]) -> Result<(), TransportError> {
        match route(target, &self.sysid_peer, &self.peers, Instant::now()) {
            Some(addr) => self.send_datagram(vec![addr], data),
            None => self.write_bytes(data),
        }
    }

    fn set_read_timeout(&mut self, timeout: Duration) {
        let _ = self.socket.set_read_timeout(Some(timeout));
    }

    fn diagnostic_note(&self) -> Option<String> {
        self.bind_note.clone()
    }

    fn description(&self) -> String {
        format!("UDP({})", self.configured)
    }
}

impl UdpTransport {
    fn send_datagram(&mut self, targets: Vec<SocketAddr>, data: &[u8]) -> Result<(), TransportError> {
        // A datagram is delivered when at least one target took it. A peer that is gone (ICMP "port
        // unreachable" surfacing on the send side, see `is_peer_unreachable`) is not a failure: the
        // datagram is simply lost, like on any quiet link, and the peer ages out on its own. A fatal
        // `Io` here would make the MSP scheduler tear the link down (`mark_lost`) and the MAVLink
        // handler warn once per heartbeat / RC frame. Only a real socket error on EVERY target is
        // reported — with several peers, one dead one must not hide that the others were reached.
        let mut delivered = 0usize;
        let mut first_err = None;
        for addr in targets {
            match self.socket.send_to(data, addr) {
                Ok(_) => delivered += 1,
                Err(ref e) if Self::is_peer_unreachable(e) => self.note_peer_unreachable("send", addr, e),
                Err(e) => {
                    log::debug!("UDP send to {} failed: {}", addr, e);
                    first_err.get_or_insert_with(|| TransportError::Io(format!("UDP send to {} failed: {}", addr, e)));
                }
            }
        }
        match first_err {
            Some(e) if delivered == 0 => Err(e),
            _ => Ok(()),
        }
    }
}

/// System ids of the MAVLink frames in one datagram (v1 and v2, signed v2 included), in order, without
/// repeats. Anything that is not a MAVLink frame (MSP over UDP, noise) yields nothing — only the header
/// is read, enough to route; the handler's parser validates the frames.
fn mavlink_sysids(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let (hdr, sysid_at) = match data[i] {
            0xFD => (10, 5),
            0xFE => (6, 3),
            _ => break,
        };
        if i + hdr > data.len() {
            break;
        }
        let mut total = hdr + data[i + 1] as usize + 2;
        if data[i] == 0xFD && data[i + 2] & 0x01 != 0 {
            total += 13; // signature
        }
        let sysid = data[i + sysid_at];
        if !out.contains(&sysid) {
            out.push(sysid);
        }
        i += total;
    }
    out
}

/// Where a frame for MAVLink system `target` goes: that system's peer while it is live; `None` (= fan out
/// to every peer) for broadcasts (`None` / 0), a system not heard yet, or one whose peer went silent.
fn route(
    target: Option<u8>,
    sysid_peer: &HashMap<u8, SocketAddr>,
    peers: &HashMap<SocketAddr, Instant>,
    now: Instant,
) -> Option<SocketAddr> {
    let addr = *sysid_peer.get(&target.filter(|t| *t != 0)?)?;
    let seen = peers.get(&addr)?;
    (now.duration_since(*seen) < PEER_TTL).then_some(addr)
}

#[cfg(test)]
mod routing_tests {
    use super::*;

    /// A MAVLink v2 frame header with an empty payload — enough for routing (no CRC check here).
    fn v2(sysid: u8) -> Vec<u8> {
        vec![0xFD, 0, 0, 0, 0, sysid, 1, 0, 0, 0, 0, 0]
    }

    fn v1(sysid: u8, payload: usize) -> Vec<u8> {
        let mut f = vec![0xFE, payload as u8, 0, sysid, 1, 0];
        f.extend(std::iter::repeat_n(0u8, payload + 2));
        f
    }

    #[test]
    fn sysids_of_every_frame_in_a_datagram() {
        let mut d = v2(3);
        d.extend(v1(7, 9));
        d.extend(v2(3));
        assert_eq!(mavlink_sysids(&d), vec![3, 7]);
    }

    #[test]
    fn signed_v2_frames_are_skipped_whole() {
        let mut signed = vec![0xFD, 0, 0x01, 0, 0, 4, 1, 0, 0, 0, 0, 0];
        signed.extend([0u8; 13]);
        signed.extend(v2(5));
        assert_eq!(mavlink_sysids(&signed), vec![4, 5]);
    }

    #[test]
    fn non_mavlink_and_truncated_data_yield_nothing() {
        assert!(mavlink_sysids(b"$X<\x00\x64\x00\x00\x00\x8f").is_empty()); // MSP v2 request
        assert!(mavlink_sysids(&[0xFD, 0, 0]).is_empty());
        assert!(mavlink_sysids(&[]).is_empty());
    }

    #[test]
    fn route_picks_the_live_peer_of_the_target_only() {
        let a: SocketAddr = "10.0.0.1:14550".parse().unwrap();
        let now = Instant::now();
        let sysid_peer = HashMap::from([(2u8, a)]);
        let peers = HashMap::from([(a, now)]);
        assert_eq!(route(Some(2), &sysid_peer, &peers, now), Some(a));
        assert_eq!(route(None, &sysid_peer, &peers, now), None, "untargeted → fan out");
        assert_eq!(route(Some(0), &sysid_peer, &peers, now), None, "broadcast → fan out");
        assert_eq!(route(Some(9), &sysid_peer, &peers, now), None, "unknown system → fan out");
        assert_eq!(route(Some(2), &sysid_peer, &peers, now + PEER_TTL), None, "silent peer → fan out");
    }

    fn free_port() -> u16 {
        UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    fn drone() -> UdpSocket {
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.set_read_timeout(Some(Duration::from_millis(150))).unwrap();
        s
    }

    fn got(s: &UdpSocket) -> Option<Vec<u8>> {
        let mut buf = [0u8; 64];
        s.recv_from(&mut buf).ok().map(|(n, _)| buf[..n].to_vec())
    }

    #[test]
    fn targeted_frames_reach_only_their_vehicle_over_real_sockets() {
        let port = free_port();
        let mut t = UdpTransport::connect("127.0.0.1", port).unwrap();
        let (d1, d2) = (drone(), drone());
        d1.send_to(&v2(1), ("127.0.0.1", port)).unwrap();
        d2.send_to(&v2(2), ("127.0.0.1", port)).unwrap();
        let mut buf = [0u8; 512];
        let deadline = Instant::now() + Duration::from_secs(2);
        while t.sysid_peer.len() < 2 && Instant::now() < deadline {
            let _ = t.read_bytes(&mut buf);
        }
        assert_eq!(t.sysid_peer.len(), 2, "both vehicles learned");

        t.write_bytes_to(Some(2), b"for-2").unwrap();
        assert_eq!(got(&d2).as_deref(), Some(&b"for-2"[..]));
        assert_eq!(got(&d1), None, "vehicle 1 must not get vehicle 2's frame");

        t.write_bytes_to(None, b"all").unwrap();
        assert_eq!(got(&d1).as_deref(), Some(&b"all"[..]));
        assert_eq!(got(&d2).as_deref(), Some(&b"all"[..]));

        t.write_bytes_to(Some(9), b"unknown").unwrap();
        assert_eq!(got(&d1).as_deref(), Some(&b"unknown"[..]), "unknown target falls back to fan-out");
        assert_eq!(got(&d2).as_deref(), Some(&b"unknown"[..]));
    }
}
