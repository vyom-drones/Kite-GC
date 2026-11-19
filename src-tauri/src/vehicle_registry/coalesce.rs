// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

//! Fleet telemetry coalescing — keeps the UI responsive with many vehicles.
//!
//! Every MAVLink frame of every vehicle used to become its own Tauri event. With a fleet (20 vehicles ≈
//! 350 frames/s) the webview's single JS thread could not keep up: the IPC queue grew without bound and
//! the UI stopped responding. The ACTIVE vehicle still emits per frame (its HUD must stay smooth); for
//! every other vehicle the high-rate STATE events are held here — latest value per (vehicle, event) wins —
//! and published together as one [`FLEET_EVENT`] every [`FLUSH_INTERVAL`]. The frontend replays each
//! entry into the same handler the plain event would have reached, so nothing downstream changes.
//!
//! Only continuous state events are held ([`COALESCED`]); one-shot and discrete events (STATUSTEXT,
//! discovery, connect answers, home, recording) always go out immediately, in order.

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex, Once};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

/// The batched event: `{ events: [{ event, payload }] }`, payloads already stamped with `vehicleId`.
pub const FLEET_EVENT: &str = "fleet-telemetry";

/// Publish rate of the held state (5 Hz) — what a fleet list / map marker needs, at a fixed IPC cost.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(200);

/// Continuous state events of a vehicle: a newer value fully replaces an older one, so holding only the
/// latest loses nothing a non-active vehicle's display needs.
const COALESCED: &[&str] = &[
    "telemetry-airspeed",
    "telemetry-altitude",
    "telemetry-analog",
    "telemetry-attitude",
    "telemetry-batteries",
    "telemetry-camera-fov",
    "telemetry-ekf-status",
    "telemetry-fc-link",
    "telemetry-flightmode",
    "telemetry-gps",
    "telemetry-gps-receivers",
    "telemetry-gps-stats",
    "telemetry-linkstats",
    "telemetry-misc2",
    "telemetry-nav-status",
    "telemetry-rc-channels",
    "telemetry-sensor-status",
    "telemetry-status",
    "telemetry-wind",
];

fn coalesced_name(event: &str) -> Option<&'static str> {
    COALESCED.iter().copied().find(|e| *e == event)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BatchEntry {
    pub event: &'static str,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize)]
struct Batch {
    events: Vec<BatchEntry>,
}

/// The held state and the active-vehicle mirror. Pure: no runtime, so it is unit-tested directly.
#[derive(Default)]
pub struct Coalescer {
    /// Wire key of the active vehicle (`"L1:S1"`), mirrored from `LinkRegistry` on every change.
    active: Option<String>,
    /// vehicle key → event → latest payload. BTreeMaps keep the published order deterministic.
    pending: BTreeMap<String, BTreeMap<&'static str, Value>>,
}

impl Coalescer {
    /// Hold `payload` if `event` is a state event of a non-active vehicle; otherwise hand it back so the
    /// caller emits it directly.
    pub fn offer(&mut self, vehicle: &str, event: &str, payload: Value) -> Option<Value> {
        if self.active.as_deref() == Some(vehicle) {
            return Some(payload);
        }
        match coalesced_name(event) {
            Some(name) => {
                self.pending.entry(vehicle.to_string()).or_default().insert(name, payload);
                None
            }
            None => Some(payload),
        }
    }

    /// Follow the active selection. The newly active vehicle's held state is dropped: its frames now go
    /// out directly, and an older batch arriving after them would roll its widgets back.
    pub fn set_active(&mut self, key: Option<String>) {
        if let Some(k) = &key {
            self.pending.remove(k);
        }
        self.active = key;
    }

    /// Take everything held, vehicle by vehicle.
    pub fn drain(&mut self) -> Vec<BatchEntry> {
        std::mem::take(&mut self.pending)
            .into_values()
            .flat_map(|events| events.into_iter().map(|(event, payload)| BatchEntry { event, payload }))
            .collect()
    }
}

static COALESCER: LazyLock<Mutex<Coalescer>> = LazyLock::new(|| Mutex::new(Coalescer::default()));
static FLUSHER: Once = Once::new();

/// Emit-path hook: `Some(payload)` → emit it now; `None` → held for the next fleet batch.
pub fn offer(app: &AppHandle, vehicle: &str, event: &str, payload: Value) -> Option<Value> {
    let held = {
        let Ok(mut c) = COALESCER.lock() else { return Some(payload) };
        c.offer(vehicle, event, payload)
    };
    if held.is_none() {
        start_flusher(app);
    }
    held
}

/// Mirror a change of the active vehicle (called by `LinkRegistry` wherever its selection changes).
pub fn set_active(key: Option<String>) {
    if let Ok(mut c) = COALESCER.lock() {
        c.set_active(key);
    }
}

/// One publisher thread for the app's lifetime, started by the first held event.
fn start_flusher(app: &AppHandle) {
    FLUSHER.call_once(|| {
        let app = app.clone();
        let spawned = std::thread::Builder::new().name("fleet-telemetry".into()).spawn(move || loop {
            std::thread::sleep(FLUSH_INTERVAL);
            let events = match COALESCER.lock() {
                Ok(mut c) => c.drain(),
                Err(_) => continue,
            };
            if !events.is_empty() {
                let _ = app.emit(FLEET_EVENT, Batch { events });
            }
        });
        match spawned {
            Ok(_) => log::info!("Fleet telemetry batching started ({} ms)", FLUSH_INTERVAL.as_millis()),
            Err(e) => log::warn!("Fleet telemetry batching thread failed to start: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn with_active(key: &str) -> Coalescer {
        let mut c = Coalescer::default();
        c.set_active(Some(key.into()));
        c
    }

    #[test]
    fn active_vehicle_is_never_held() {
        let mut c = with_active("L1:S1");
        assert_eq!(c.offer("L1:S1", "telemetry-gps", json!({"lat": 1})), Some(json!({"lat": 1})));
        assert!(c.drain().is_empty());
    }

    #[test]
    fn state_event_of_other_vehicle_is_held_latest_wins() {
        let mut c = with_active("L1:S1");
        assert_eq!(c.offer("L1:S2", "telemetry-gps", json!({"lat": 1})), None);
        assert_eq!(c.offer("L1:S2", "telemetry-gps", json!({"lat": 2})), None);
        assert_eq!(c.drain(), vec![BatchEntry { event: "telemetry-gps", payload: json!({"lat": 2}) }]);
        assert!(c.drain().is_empty(), "drain empties the buffer");
    }

    #[test]
    fn discrete_events_always_pass_through() {
        let mut c = with_active("L1:S1");
        for ev in ["mavlink-statustext", "vehicle-discovered", "home-position", "telemetry-ekf-type", "telemetry-vehicle"] {
            assert_eq!(c.offer("L1:S2", ev, json!({"x": 1})), Some(json!({"x": 1})), "{ev}");
        }
        assert!(c.drain().is_empty());
    }

    #[test]
    fn every_vehicle_and_event_keeps_its_own_slot() {
        let mut c = with_active("L1:S1");
        c.offer("L1:S3", "telemetry-gps", json!(3));
        c.offer("L1:S2", "telemetry-gps", json!(2));
        c.offer("L1:S2", "telemetry-attitude", json!(22));
        let got: Vec<(&str, Value)> = c.drain().into_iter().map(|e| (e.event, e.payload)).collect();
        assert_eq!(got, vec![("telemetry-attitude", json!(22)), ("telemetry-gps", json!(2)), ("telemetry-gps", json!(3))]);
    }

    #[test]
    fn becoming_active_drops_that_vehicles_held_state() {
        let mut c = with_active("L1:S1");
        c.offer("L1:S2", "telemetry-gps", json!(2));
        c.offer("L1:S3", "telemetry-gps", json!(3));
        c.set_active(Some("L1:S2".into()));
        assert_eq!(c.drain(), vec![BatchEntry { event: "telemetry-gps", payload: json!(3) }]);
        assert_eq!(c.offer("L1:S1", "telemetry-gps", json!(1)), None, "the previously active one is held now");
    }

    #[test]
    fn no_active_vehicle_holds_state_events() {
        let mut c = Coalescer::default();
        assert_eq!(c.offer("L1:S1", "telemetry-status", json!(1)), None);
        assert_eq!(c.drain().len(), 1);
    }
}
