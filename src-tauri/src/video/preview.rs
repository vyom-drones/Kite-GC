// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

//! Fleet video preview sessions — many small live tiles at once (the fleet video grid, map popups).
//!
//! The main video path (`rtsp_native`) is one stream into a native decode sink. A grid of 20+ drones needs
//! the opposite: many low-rate streams, each a plain multipart MJPEG endpoint the webview shows in an
//! `<img>`. One session per stream id (`"L1:S7:preview"`), each with Kite's own RTSP client and its own
//! local HTTP port (the `mjpeg_server` accept/broadcast machinery):
//!
//! * **MJPEG** preview from the drone → the frames pass straight through (no ffmpeg).
//! * **H.264 / H.265** preview → re-encoded to small JPEGs by ffmpeg (`-use_wallclock_as_timestamps`:
//!   raw Annex-B has no frame timing, and the fps filter must not assume 25 fps). Without ffmpeg the
//!   session fails with a message saying so.
//!
//! Sessions are started and stopped by the frontend as tiles come on and off screen, so only watched
//! streams use the radio link. Several windows (the main window's grid, the separate fleet video window)
//! share one session per stream — one RTSP pull per drone — each as an OWNER: a session stops when its
//! last owner lets go, so closing one window's tile never blanks the other's.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Serialize;

use super::mjpeg_server::{accept_loop, broadcast_loop, Client, EndedHook};
use super::rtsp::{run_rtsp, LiveRtspStats, RtspConfig, RtspTransport, VideoCodec};
use super::rtsp_native::{mpjpeg_part, ChannelRead};

/// Upper bound on simultaneous preview sessions (a full fleet grid plus headroom).
pub const MAX_PREVIEW_SESSIONS: usize = 32;
/// RTSP negotiation (incl. a possible UDP→TCP fallback) plus the first picture.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(12);
/// What an H.264/H.265 preview is re-encoded to.
const TRANSCODE_WIDTH: u32 = 320;
const TRANSCODE_FPS: u32 = 5;

/// How `start` treats a stream id, given what already runs. Pure (unit-tested).
#[derive(Debug, PartialEq)]
pub(crate) enum StartPlan {
    /// Same id, same URL: hand back the running session's port.
    Reuse(u16),
    /// Same id, another URL (the template changed): replace it.
    Replace,
    New,
    /// At `MAX_PREVIEW_SESSIONS`.
    Full,
}

pub(crate) fn plan_start(existing: Option<(&str, u16)>, url: &str, running: usize, max: usize) -> StartPlan {
    match existing {
        Some((u, port)) if u == url => StartPlan::Reuse(port),
        Some(_) => StartPlan::Replace,
        None if running >= max => StartPlan::Full,
        None => StartPlan::New,
    }
}

/// ffmpeg arguments: Annex-B on stdin → small MJPEG parts (mpjpeg, the broadcast's framing) on stdout.
pub(crate) fn transcode_args(codec: VideoCodec) -> Vec<String> {
    let input = if codec == VideoCodec::H265 { "hevc" } else { "h264" };
    let filter = format!("scale={TRANSCODE_WIDTH}:-2,fps={TRANSCODE_FPS}");
    [
        "-hide_banner", "-loglevel", "error", "-fflags", "nobuffer", "-flags", "low_delay",
        "-use_wallclock_as_timestamps", "1", "-f", input, "-i", "pipe:0", "-an", "-vf", &filter,
        "-q:v", "7", "-flush_packets", "1", "-f", "mpjpeg", "pipe:1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

pub(crate) const ERR_NEEDS_FFMPEG: &str =
    "This preview is H.264/H.265 and needs ffmpeg on the ground station — install ffmpeg, or serve an MJPEG preview";

/// One session's live numbers, for the frontend's tile status and the Debug Monitor.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewStats {
    pub stream_id: String,
    pub url: String,
    /// "MJPEG" (passed through) or "H.264→MJPEG" / "H.265→MJPEG" (transcoded); None before the first frame.
    pub codec: Option<&'static str>,
    pub frames: u64,
    pub bytes: u64,
    pub rtp_lost: u64,
}

struct Session {
    url: String,
    /// Who watches it (window labels). Empty → stopped.
    owners: HashSet<String>,
    port: u16,
    stop: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    rtsp: JoinHandle<()>,
    broadcast: JoinHandle<()>,
    accept: JoinHandle<()>,
    transcoder: Arc<Mutex<Option<Child>>>,
    live: Arc<LiveRtspStats>,
    codec: Arc<Mutex<Option<&'static str>>>,
}

/// Same order as `rtsp_native::teardown` (no spurious "ended" event, no deadlock), plus the transcoder:
/// killing it ends its stdout pump, whose frame sender is the broadcast's last open input.
fn teardown(s: Session) {
    s.shutdown.store(true, Ordering::SeqCst);
    s.stop.store(true, Ordering::SeqCst);
    if let Some(mut child) = s.transcoder.lock().ok().and_then(|mut c| c.take()) {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = s.rtsp.join();
    let _ = s.broadcast.join();
    let _ = s.accept.join();
}

#[derive(Default)]
pub struct PreviewSessions {
    sessions: Mutex<HashMap<String, Session>>,
}

impl PreviewSessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start (or join) the preview session `stream_id` on `url` for `owner` (a window label); returns its
    /// local multipart port once the first picture arrived. `on_ended` fires when a live session's source
    /// dies — never on `stop`. A URL change restarts the session for every owner.
    pub fn start(&self, stream_id: &str, owner: &str, url: &str, transport: &str, on_ended: EndedHook) -> Result<u16, String> {
        let mut owners = HashSet::from([owner.to_string()]);
        let replaced = {
            let mut map = self.sessions.lock().map_err(|e| e.to_string())?;
            let existing = map.get(stream_id).map(|s| (s.url.as_str(), s.port));
            match plan_start(existing, url, map.len(), MAX_PREVIEW_SESSIONS) {
                StartPlan::Reuse(port) => {
                    if let Some(s) = map.get_mut(stream_id) {
                        s.owners.insert(owner.to_string());
                    }
                    return Ok(port);
                }
                StartPlan::Full => return Err(format!("Too many preview streams (max {MAX_PREVIEW_SESSIONS})")),
                StartPlan::Replace => map.remove(stream_id),
                StartPlan::New => None,
            }
        };
        if let Some(mut old) = replaced {
            owners.extend(old.owners.drain());
            teardown(old);
        }
        let mut session = spawn_session(url, transport, on_ended)?;
        session.owners = owners;
        let port = session.port;
        log::info!("Video preview {stream_id}: live on port {port} ({url})");
        // A concurrent start of the same id may have finished first — keep the newest, with both owner sets.
        let mut map = self.sessions.lock().map_err(|e| e.to_string())?;
        if let Some(mut older) = map.remove(stream_id) {
            session.owners.extend(older.owners.drain());
            drop(map);
            teardown(older);
            map = self.sessions.lock().map_err(|e| e.to_string())?;
        }
        map.insert(stream_id.to_string(), session);
        Ok(port)
    }

    /// `owner` stops watching `stream_id`; the session ends with its last owner.
    pub fn stop(&self, stream_id: &str, owner: &str) {
        let ended = self.sessions.lock().ok().and_then(|mut m| {
            let s = m.get_mut(stream_id)?;
            s.owners.remove(owner);
            if s.owners.is_empty() { m.remove(stream_id) } else { None }
        });
        if let Some(s) = ended {
            log::debug!("Video preview {stream_id}: stopped");
            teardown(s);
        }
    }

    /// `owner` (a closing window) stops watching everything.
    pub fn stop_owner(&self, owner: &str) {
        let ended: Vec<Session> = match self.sessions.lock() {
            Ok(mut m) => {
                for s in m.values_mut() {
                    s.owners.remove(owner);
                }
                let gone: Vec<String> = m.iter().filter(|(_, s)| s.owners.is_empty()).map(|(k, _)| k.clone()).collect();
                gone.iter().filter_map(|k| m.remove(k)).collect()
            }
            Err(_) => return,
        };
        if !ended.is_empty() {
            log::debug!("Video preview: {owner} closed — {} session(s) stopped", ended.len());
        }
        for s in ended {
            teardown(s);
        }
    }

    pub fn stop_all(&self) {
        let all: Vec<Session> = match self.sessions.lock() {
            Ok(mut m) => m.drain().map(|(_, s)| s).collect(),
            Err(_) => return,
        };
        for s in all {
            teardown(s);
        }
    }

    pub fn stats(&self) -> Vec<PreviewStats> {
        use std::sync::atomic::Ordering::Relaxed;
        let Ok(map) = self.sessions.lock() else { return Vec::new() };
        let mut out: Vec<PreviewStats> = map
            .iter()
            .map(|(id, s)| PreviewStats {
                stream_id: id.clone(),
                url: s.url.clone(),
                codec: s.codec.lock().ok().and_then(|c| *c),
                frames: s.live.frames.load(Relaxed),
                bytes: s.live.bytes.load(Relaxed),
                rtp_lost: s.live.rtp_lost.load(Relaxed),
            })
            .collect();
        out.sort_by(|a, b| a.stream_id.cmp(&b.stream_id));
        out
    }
}

impl Drop for PreviewSessions {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// The ffmpeg transcoder of an H.264/H.265 session: its stdin takes access units; a pump thread forwards
/// its stdout (mpjpeg parts) into the session's frame channel.
fn spawn_transcoder(codec: VideoCodec, frame_tx: Sender<Vec<u8>>) -> Result<(Child, std::process::ChildStdin), String> {
    let bin = super::ffmpeg::find_ffmpeg().ok_or_else(|| ERR_NEEDS_FFMPEG.to_string())?;
    let mut cmd = Command::new(bin);
    crate::child_env::sanitize(&mut cmd);
    cmd.args(transcode_args(codec)).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|e| format!("ffmpeg spawn failed: {e}"))?;
    let stdin = child.stdin.take().ok_or("ffmpeg: no stdin")?;
    let mut stdout = child.stdout.take().ok_or("ffmpeg: no stdout")?;
    thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 || frame_tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    Ok((child, stdin))
}

fn spawn_session(url: &str, transport: &str, on_ended: EndedHook) -> Result<Session, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| format!("set_nonblocking: {e}"))?;
    let port = listener.local_addr().map_err(|e| format!("addr: {e}"))?.port();

    let live = Arc::new(LiveRtspStats::default());
    let cfg = RtspConfig {
        url: url.to_string(),
        transport: match transport {
            "udp" => RtspTransport::Udp,
            "tcp" => RtspTransport::Tcp,
            _ => RtspTransport::Auto,
        },
        accept: vec![VideoCodec::Mjpeg, VideoCodec::H264, VideoCodec::H265],
        live: Some(live.clone()),
        ..Default::default()
    };

    let stop = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));
    let clients: Arc<Mutex<Vec<Client>>> = Arc::new(Mutex::new(Vec::new()));
    let (frame_tx, frame_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let (first_tx, first_rx) = std::sync::mpsc::channel::<()>();
    let error_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let transcoder: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
    let codec: Arc<Mutex<Option<&'static str>>> = Arc::new(Mutex::new(None));

    let accept = {
        let (clients, shutdown) = (clients.clone(), shutdown.clone());
        thread::spawn(move || accept_loop(listener, clients, shutdown))
    };
    let broadcast = {
        let shutdown = shutdown.clone();
        thread::spawn(move || broadcast_loop(ChannelRead::new(frame_rx), clients, shutdown, Some(first_tx), on_ended))
    };
    let rtsp = {
        let (stop, error_slot, transcoder, codec) = (stop.clone(), error_slot.clone(), transcoder.clone(), codec.clone());
        thread::spawn(move || {
            let mut first = true;
            let mut ffmpeg_in: Option<std::process::ChildStdin> = None;
            // The first error wins: it is what `start` reports.
            let record = |e: String| {
                if let Ok(mut s) = error_slot.lock() {
                    s.get_or_insert(e);
                }
            };
            let fail = |e: String, stop: &AtomicBool| {
                record(e);
                stop.store(true, Ordering::SeqCst);
            };
            let result = run_rtsp(&cfg, &stop, &mut |frame| match frame.codec {
                VideoCodec::Mjpeg => {
                    if first {
                        *codec.lock().unwrap() = Some("MJPEG");
                    }
                    let _ = frame_tx.send(mpjpeg_part(&frame.data, first));
                    first = false;
                }
                VideoCodec::H264 | VideoCodec::H265 => {
                    if ffmpeg_in.is_none() {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        match spawn_transcoder(frame.codec, frame_tx.clone()) {
                            Ok((child, stdin)) => {
                                *transcoder.lock().unwrap() = Some(child);
                                *codec.lock().unwrap() =
                                    Some(if frame.codec == VideoCodec::H265 { "H.265→MJPEG" } else { "H.264→MJPEG" });
                                ffmpeg_in = Some(stdin);
                            }
                            Err(e) => return fail(e, &stop),
                        }
                    }
                    if let Some(w) = ffmpeg_in.as_mut() {
                        if let Err(e) = w.write_all(&frame.data) {
                            ffmpeg_in = None;
                            fail(format!("ffmpeg transcode stopped: {e}"), &stop);
                        }
                    }
                }
            });
            if let Err(e) = result {
                record(e);
            }
        })
    };

    let session = Session {
        url: url.to_string(), owners: HashSet::new(), port, stop, shutdown, rtsp, broadcast, accept, transcoder, live, codec,
    };
    match first_rx.recv_timeout(FIRST_FRAME_TIMEOUT) {
        Ok(()) => Ok(session),
        Err(e) => {
            let msg = error_slot.lock().ok().and_then(|s| s.clone()).unwrap_or_else(|| match e {
                RecvTimeoutError::Timeout => "No picture from the preview stream (timed out)".to_string(),
                RecvTimeoutError::Disconnected => "The preview stream ended before the first picture".to_string(),
            });
            teardown(session);
            log::warn!("Video preview failed to start ({url}): {msg}");
            Err(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_reuses_replaces_adds_or_refuses() {
        assert_eq!(plan_start(Some(("rtsp://a", 4100)), "rtsp://a", 3, 32), StartPlan::Reuse(4100));
        assert_eq!(plan_start(Some(("rtsp://a", 4100)), "rtsp://b", 3, 32), StartPlan::Replace);
        assert_eq!(plan_start(None, "rtsp://a", 3, 32), StartPlan::New);
        assert_eq!(plan_start(None, "rtsp://a", 32, 32), StartPlan::Full);
        assert_eq!(plan_start(Some(("rtsp://a", 1)), "rtsp://b", 32, 32), StartPlan::Replace, "a replacement never grows the map");
    }

    #[test]
    fn transcode_args_read_stdin_with_wallclock_timing_and_write_mpjpeg() {
        let a = transcode_args(VideoCodec::H264);
        let at = |k: &str| a.iter().position(|x| x == k).map(|i| a[i + 1].as_str());
        assert_eq!(at("-use_wallclock_as_timestamps"), Some("1"));
        assert_eq!(at("-f"), Some("h264"), "the first -f is the input format");
        assert_eq!(at("-i"), Some("pipe:0"));
        assert_eq!(at("-vf"), Some("scale=320:-2,fps=5"));
        assert_eq!(a.last().map(String::as_str), Some("pipe:1"));
        assert!(a.windows(2).any(|w| w == ["-f", "mpjpeg"]));
        let wall = a.iter().position(|x| x == "-use_wallclock_as_timestamps").unwrap();
        assert!(wall < a.iter().position(|x| x == "-i").unwrap(), "an input option must precede -i");
        assert_eq!(transcode_args(VideoCodec::H265)[a.iter().position(|x| x == "-f").unwrap() + 1], "hevc");
    }

    #[test]
    fn unreachable_source_fails_fast_and_leaves_no_session() {
        let previews = PreviewSessions::new();
        let err = previews.start("t:preview", "main", "rtsp://127.0.0.1:1/none", "tcp", Arc::new(|| {})).unwrap_err();
        assert!(!err.is_empty());
        assert!(previews.stats().is_empty());
    }

    /// Against live sources: `KITE_PREVIEW_URLS="rtsp://…/drone1/preview rtsp://…/drone2/preview"` (e.g.
    /// tools/simulators/fleet_video_server.py). Several sessions at once, each serving multipart JPEGs.
    #[test]
    fn serves_several_live_previews_at_once() {
        let Ok(urls) = std::env::var("KITE_PREVIEW_URLS") else {
            eprintln!("KITE_PREVIEW_URLS not set — skipping");
            return;
        };
        let urls: Vec<&str> = urls.split_whitespace().collect();
        let previews = PreviewSessions::new();
        let ports: Vec<u16> = urls
            .iter()
            .enumerate()
            .map(|(i, u)| previews.start(&format!("t{i}"), "main", u, "auto", Arc::new(|| {})).expect("start"))
            .collect();
        // A second window joins the first stream: same session, same port.
        assert_eq!(previews.start("t0", "fleet-video", urls[0], "auto", Arc::new(|| {})).unwrap(), ports[0], "same id + url reuses");
        for port in &ports {
            let mut sock = std::net::TcpStream::connect(("127.0.0.1", *port)).expect("connect");
            sock.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut buf = vec![0u8; 256 * 1024];
            let mut got = 0;
            while got < 60_000 {
                match sock.read(&mut buf[got..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => got += n,
                }
            }
            let parts = buf[..got].windows(8).filter(|w| *w == b"--ffmpeg").count();
            assert!(parts >= 2, "port {port}: expected several JPEG parts, got {parts}");
        }
        assert_eq!(previews.stats().len(), urls.len());
        previews.stop("t0", "main");
        assert_eq!(previews.stats().len(), urls.len(), "t0 still has the other window as owner");
        previews.stop_owner("fleet-video");
        assert_eq!(previews.stats().len(), urls.len() - 1, "its last owner closed");
        previews.stop("t1", "nobody");
        assert_eq!(previews.stats().len(), urls.len() - 1, "a stranger's stop changes nothing");
        previews.stop_all();
        assert!(previews.stats().is_empty());
    }
}
