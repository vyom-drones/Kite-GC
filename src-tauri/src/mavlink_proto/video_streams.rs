// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

//! MAVLink camera protocol — video stream discovery for fleet video.
//!
//! A camera manager on the drone's companion (BlueOS mavlink-camera-manager, or a camera speaking MAVLink)
//! advertises its streams in VIDEO_STREAM_INFORMATION (#269): name, URI, resolution, encoding. Kite asks
//! every vehicle with MAV_CMD_REQUEST_MESSAGE addressed to component 0 (all components of the system), so
//! the camera answers even though it is not the autopilot. Each answer is one stream; the frontend
//! collects them per vehicle and uses an advertised RTSP URI when the operator set no URL of their own.

use ::mavlink::ardupilotmega::{MavCmd, MavMessage, VideoStreamType, COMMAND_LONG_DATA, VIDEO_STREAM_INFORMATION_DATA};
use serde::Serialize;

/// VIDEO_STREAM_INFORMATION's message id.
const MSG_ID_VIDEO_STREAM_INFORMATION: f32 = 269.0;

/// MAV_CMD_REQUEST_MESSAGE for every stream (param2 = 0) of every component of `sysid`.
pub(crate) fn video_stream_request(sysid: u8) -> MavMessage {
    MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
        target_system: sysid,
        target_component: 0,
        command: MavCmd::MAV_CMD_REQUEST_MESSAGE,
        confirmation: 0,
        param1: MSG_ID_VIDEO_STREAM_INFORMATION,
        param2: 0.0,
        param3: 0.0,
        param4: 0.0,
        param5: 0.0,
        param6: 0.0,
        param7: 0.0,
    })
}

/// One advertised stream, as the frontend receives it (`telemetry-video-stream`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoStream {
    /// The component that advertised it (a camera is usually 100+).
    pub compid: u8,
    pub stream_id: u8,
    /// Streams this camera has in total.
    pub count: u8,
    /// "rtsp" | "rtpudp" | "tcp-mpeg" | "mpeg-ts" — only "rtsp" is something Kite can pull.
    pub kind: &'static str,
    pub name: String,
    pub uri: String,
    pub width: u16,
    pub height: u16,
    pub framerate: f32,
    /// bit/s; 0 = unknown.
    pub bitrate: u32,
    /// "h264" | "h265" | "unknown" (MJPEG cameras report unknown).
    pub encoding: &'static str,
}

fn c_string(bytes: &[u8]) -> String {
    bytes.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect::<String>().trim().to_string()
}

/// VIDEO_STREAM_INFORMATION → `VideoStream`. Pure.
pub(crate) fn parse_video_stream(compid: u8, d: &VIDEO_STREAM_INFORMATION_DATA) -> VideoStream {
    VideoStream {
        compid,
        stream_id: d.stream_id,
        count: d.count,
        kind: match d.mavtype {
            VideoStreamType::VIDEO_STREAM_TYPE_RTSP => "rtsp",
            VideoStreamType::VIDEO_STREAM_TYPE_RTPUDP => "rtpudp",
            VideoStreamType::VIDEO_STREAM_TYPE_TCP_MPEG => "tcp-mpeg",
            VideoStreamType::VIDEO_STREAM_TYPE_MPEG_TS => "mpeg-ts",
        },
        name: c_string(&d.name[..]),
        uri: c_string(&d.uri[..]),
        width: d.resolution_h,
        height: d.resolution_v,
        framerate: d.framerate,
        bitrate: d.bitrate,
        encoding: match d.encoding as u8 {
            1 => "h264",
            2 => "h265",
            _ => "unknown",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars<const N: usize>(s: &str) -> [u8; N] {
        let mut a = [0u8; N];
        a[..s.len()].copy_from_slice(s.as_bytes());
        a
    }

    #[test]
    fn request_asks_every_component_for_all_streams() {
        let MavMessage::COMMAND_LONG(c) = video_stream_request(7) else { panic!("not a COMMAND_LONG") };
        assert_eq!((c.target_system, c.target_component), (7, 0));
        assert_eq!(c.command, MavCmd::MAV_CMD_REQUEST_MESSAGE);
        assert_eq!((c.param1, c.param2), (269.0, 0.0));
    }

    #[test]
    fn parses_an_rtsp_stream() {
        let d = VIDEO_STREAM_INFORMATION_DATA {
            stream_id: 2,
            count: 2,
            mavtype: VideoStreamType::VIDEO_STREAM_TYPE_RTSP,
            framerate: 5.0,
            resolution_h: 320,
            resolution_v: 176,
            bitrate: 300_000,
            name: chars::<32>("preview").into(),
            uri: chars::<160>("rtsp://10.10.0.7:8554/preview").into(),
            ..Default::default()
        };
        let s = parse_video_stream(100, &d);
        assert_eq!(s.kind, "rtsp");
        assert_eq!((s.compid, s.stream_id, s.count), (100, 2, 2));
        assert_eq!(s.name, "preview");
        assert_eq!(s.uri, "rtsp://10.10.0.7:8554/preview");
        assert_eq!((s.width, s.height, s.bitrate), (320, 176, 300_000));
        assert_eq!(s.encoding, "unknown");
    }

    #[test]
    fn a_full_length_uri_without_terminator_is_kept_whole() {
        let long = format!("rtsp://h/{}", "x".repeat(151));
        assert_eq!(long.len(), 160);
        let d = VIDEO_STREAM_INFORMATION_DATA { uri: chars::<160>(&long).into(), ..Default::default() };
        assert_eq!(parse_video_stream(100, &d).uri, long);
    }

    #[test]
    fn non_rtsp_streams_are_labelled() {
        let d = VIDEO_STREAM_INFORMATION_DATA { mavtype: VideoStreamType::VIDEO_STREAM_TYPE_RTPUDP, ..Default::default() };
        assert_eq!(parse_video_stream(100, &d).kind, "rtpudp");
    }
}
