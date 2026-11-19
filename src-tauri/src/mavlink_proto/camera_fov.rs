// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

//! Gimbal target — where a vehicle's camera looks (CAMERA_FOV_STATUS, #271).
//!
//! ArduPilot's camera backend (any mount: SIYI A8, Topotek, servo gimbal, …) reports the camera's position
//! and its earth-frame attitude, and — when the mount could ray-cast the line of sight into its terrain
//! database (TERRAIN_ENABLE with terrain data on the FC) — the point of interest where that line meets the
//! ground. A missing point arrives as INT32_MAX. The frontend shows ArduPilot's point when there is one and
//! otherwise estimates it from the camera position and the line-of-sight pitch / yaw derived here.

use ::mavlink::ardupilotmega::CAMERA_FOV_STATUS_DATA;
use serde::Serialize;

/// "Not known" in CAMERA_FOV_STATUS's integer fields.
const UNKNOWN: i32 = i32::MAX;

/// What the frontend receives (`telemetry-camera-fov`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraFov {
    /// Camera position (deg, deg, m MSL); None when the FC has no position.
    pub cam_lat: Option<f64>,
    pub cam_lon: Option<f64>,
    pub cam_alt_msl: Option<f64>,
    /// ArduPilot's point of interest on the terrain; None when it could not compute one.
    pub poi_lat: Option<f64>,
    pub poi_lon: Option<f64>,
    pub poi_alt_msl: Option<f64>,
    /// Line of sight in the earth frame: pitch (deg, negative = below the horizon) and yaw (deg true, 0..360).
    pub pitch_deg: Option<f64>,
    pub yaw_deg: Option<f64>,
    /// Field of view (deg); None when the camera's FOV parameters are not set.
    pub hfov_deg: Option<f64>,
    pub vfov_deg: Option<f64>,
}

fn deg(e7: i32) -> Option<f64> {
    (e7 != UNKNOWN).then(|| e7 as f64 / 1e7)
}

fn metres(mm: i32) -> Option<f64> {
    (mm != UNKNOWN).then(|| mm as f64 / 1000.0)
}

fn positive(v: f32) -> Option<f64> {
    (v.is_finite() && v > 0.0).then_some(v as f64)
}

/// Earth-frame quaternion [w, x, y, z] (NED) → (pitch, yaw) in degrees; None for a zero/invalid quaternion.
pub(crate) fn pitch_yaw(q: [f32; 4]) -> Option<(f64, f64)> {
    let [w, x, y, z] = q.map(|v| v as f64);
    let n = (w * w + x * x + y * y + z * z).sqrt();
    if !n.is_finite() || n < 1e-6 {
        return None;
    }
    let (w, x, y, z) = (w / n, x / n, y / n, z / n);
    let pitch = (2.0 * (w * y - z * x)).clamp(-1.0, 1.0).asin().to_degrees();
    let yaw = (2.0 * (w * z + x * y)).atan2(1.0 - 2.0 * (y * y + z * z)).to_degrees();
    Some((pitch, yaw.rem_euclid(360.0)))
}

/// CAMERA_FOV_STATUS → `CameraFov`. Pure.
pub(crate) fn parse_camera_fov(d: &CAMERA_FOV_STATUS_DATA) -> CameraFov {
    let (pitch_deg, yaw_deg) = pitch_yaw(d.q).unzip();
    CameraFov {
        cam_lat: deg(d.lat_camera),
        cam_lon: deg(d.lon_camera),
        cam_alt_msl: metres(d.alt_camera),
        poi_lat: deg(d.lat_image),
        poi_lon: deg(d.lon_image),
        poi_alt_msl: metres(d.alt_image),
        pitch_deg,
        yaw_deg,
        hfov_deg: positive(d.hfov),
        vfov_deg: positive(d.vfov),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Quaternion of a yaw (Z) then pitch (Y) rotation — the order ArduPilot builds the earth-frame attitude in.
    fn q_from(pitch_deg: f64, yaw_deg: f64) -> [f32; 4] {
        let (cp, sp) = ((pitch_deg.to_radians() / 2.0).cos(), (pitch_deg.to_radians() / 2.0).sin());
        let (cy, sy) = ((yaw_deg.to_radians() / 2.0).cos(), (yaw_deg.to_radians() / 2.0).sin());
        [(cy * cp) as f32, (-sy * sp) as f32, (cy * sp) as f32, (sy * cp) as f32]
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.05
    }

    #[test]
    fn pitch_and_yaw_from_the_earth_frame_quaternion() {
        for (p, y) in [(-45.0, 90.0), (-90.0, 0.0), (0.0, 270.0), (-10.0, 359.0), (-30.0, 12.5)] {
            let (pp, yy) = pitch_yaw(q_from(p, y)).unwrap();
            assert!(close(pp, p), "pitch {p}: got {pp}");
            if p > -89.0 {
                assert!(close(yy, y), "yaw {y}: got {yy}");
            }
        }
        assert_eq!(pitch_yaw([0.0; 4]), None);
        assert_eq!(pitch_yaw([f32::NAN, 0.0, 0.0, 0.0]), None);
    }

    #[test]
    fn parses_ardupilot_point_of_interest() {
        let d = CAMERA_FOV_STATUS_DATA {
            lat_camera: -353_632_610,
            lon_camera: 1_491_652_300,
            alt_camera: 634_000,
            lat_image: -353_620_000,
            lon_image: 1_491_660_000,
            alt_image: 584_000,
            q: q_from(-45.0, 30.0),
            hfov: 81.0,
            vfov: f32::NAN,
            ..Default::default()
        };
        let f = parse_camera_fov(&d);
        assert_eq!(f.cam_lat, Some(-35.363261));
        assert_eq!(f.cam_alt_msl, Some(634.0));
        assert_eq!((f.poi_lat, f.poi_alt_msl), (Some(-35.362), Some(584.0)));
        assert!(close(f.pitch_deg.unwrap(), -45.0) && close(f.yaw_deg.unwrap(), 30.0));
        assert_eq!((f.hfov_deg, f.vfov_deg), (Some(81.0), None), "a NaN FOV is unknown");
    }

    #[test]
    fn int32_max_means_no_point_of_interest() {
        let d = CAMERA_FOV_STATUS_DATA {
            lat_camera: -353_632_610,
            lon_camera: 1_491_652_300,
            alt_camera: 634_000,
            lat_image: i32::MAX,
            lon_image: i32::MAX,
            alt_image: i32::MAX,
            q: q_from(-20.0, 0.0),
            ..Default::default()
        };
        let f = parse_camera_fov(&d);
        assert_eq!((f.poi_lat, f.poi_lon, f.poi_alt_msl), (None, None, None));
        assert!(f.cam_lat.is_some() && f.pitch_deg.is_some());
    }
}
