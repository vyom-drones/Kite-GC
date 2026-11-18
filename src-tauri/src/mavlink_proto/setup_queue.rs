// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 vyom

//! Paced, ACK-confirmed vehicle setup for multi-vehicle links.
//!
//! A vehicle discovered on a shared link gets ~30 SET_MESSAGE_INTERVAL commands plus a few one-shot
//! requests. Written all at once — for 20 vehicles tens of kB in one burst — a thin link (an RFD900
//! multipoint network: ~6 kB/s shared, a 2 kB radio buffer) dropped most of it, and the setup was
//! fire-and-forget: a vehicle whose rate commands were lost never streamed a position and stayed off the
//! map. This queue sends within a byte budget and confirms every rate command by its COMMAND_ACK, resending
//! what got lost. The ACK does not say WHICH message's interval it confirms, so a vehicle has one rate
//! command in flight at a time; vehicles proceed in parallel, served round-robin within the budget.
//! Pure state (the clock is passed in) — unit-tested below.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use ::mavlink::ardupilotmega::{MavCmd, MavMessage, MavResult};

/// How long a rate command waits for its ACK before it is sent again (an RFD queue can hold a frame for
/// several hundred ms each way).
pub(crate) const ACK_TIMEOUT: Duration = Duration::from_millis(2500);
/// Sends of one rate command before it is given up.
pub(crate) const MAX_ATTEMPTS: u8 = 3;
/// On-air size of one setup frame for the budget (a MAVLink 2 COMMAND_LONG is 45 bytes).
const FRAME_BYTES: f64 = 45.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    /// SET_MESSAGE_INTERVAL: confirmed by its ACK, resent when lost, superseded by a newer profile.
    Rate,
    /// A request whose answer is its own message (PARAM_VALUE, HOME_POSITION): paced, not confirmed.
    OneShot,
}

struct Item {
    msg: MavMessage,
    kind: Kind,
    attempts: u8,
}

struct Inflight {
    item: Item,
    sent: Instant,
}

/// How a vehicle's setup went, reported once its queue has drained.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct SetupReport {
    pub confirmed: u32,
    /// The FC answered but declined (e.g. a message this firmware does not have) — done, not resent.
    pub refused: u32,
    pub gave_up: u32,
}

pub(crate) struct SetupQueue {
    queues: BTreeMap<u8, VecDeque<Item>>,
    inflight: BTreeMap<u8, Inflight>,
    reports: BTreeMap<u8, SetupReport>,
    rate: f64,
    burst: f64,
    tokens: f64,
    last: Option<Instant>,
    /// Round-robin: the vehicle served last, so the next poll starts after it.
    cursor: Option<u8>,
}

impl SetupQueue {
    /// `rate_bps`: the uplink budget for setup traffic, bytes per second.
    pub(crate) fn new(rate_bps: f64) -> Self {
        let burst = (rate_bps * 0.5).max(FRAME_BYTES * 4.0);
        Self {
            queues: BTreeMap::new(),
            inflight: BTreeMap::new(),
            reports: BTreeMap::new(),
            rate: rate_bps,
            burst,
            tokens: burst,
            last: None,
            cursor: None,
        }
    }

    pub(crate) fn push(&mut self, sysid: u8, msg: MavMessage, kind: Kind) {
        self.queues.entry(sysid).or_default().push_back(Item { msg, kind, attempts: 0 });
        self.reports.entry(sysid).or_default();
    }

    /// A new stream profile for `sysid` (the active vehicle changed): its queued, not yet sent rate commands
    /// are superseded. The one in flight finishes; one-shot requests stay.
    pub(crate) fn replace_rates(&mut self, sysid: u8, msgs: Vec<MavMessage>) {
        let q = self.queues.entry(sysid).or_default();
        q.retain(|i| i.kind != Kind::Rate);
        q.extend(msgs.into_iter().map(|msg| Item { msg, kind: Kind::Rate, attempts: 0 }));
        self.reports.entry(sysid).or_default();
    }

    /// The vehicle left the link.
    pub(crate) fn forget(&mut self, sysid: u8) {
        self.queues.remove(&sysid);
        self.inflight.remove(&sysid);
        self.reports.remove(&sysid);
    }

    /// An ACK from `sysid`. True when it settled the in-flight rate command — the caller then does not hand
    /// it on (it is ours, not an operator command's).
    pub(crate) fn on_ack(&mut self, sysid: u8, command: MavCmd, result: MavResult) -> bool {
        if command != MavCmd::MAV_CMD_SET_MESSAGE_INTERVAL
            || !self.inflight.get(&sysid).is_some_and(|f| f.item.kind == Kind::Rate)
        {
            return false;
        }
        self.inflight.remove(&sysid);
        let r = self.reports.entry(sysid).or_default();
        match result {
            MavResult::MAV_RESULT_ACCEPTED | MavResult::MAV_RESULT_IN_PROGRESS => r.confirmed += 1,
            _ => r.refused += 1,
        }
        true
    }

    /// What to send now, within the budget: per idle vehicle its next item (a timed-out rate command
    /// first), round-robin. Also returns the vehicles whose setup just finished, with how it went.
    pub(crate) fn poll(&mut self, now: Instant) -> (Vec<(u8, MavMessage)>, Vec<(u8, SetupReport)>) {
        if let Some(last) = self.last {
            self.tokens = (self.tokens + now.duration_since(last).as_secs_f64() * self.rate).min(self.burst);
        }
        self.last = Some(now);

        // Lost (no ACK in time): back to the front of its vehicle's queue, or given up.
        let timed_out: Vec<u8> = self
            .inflight
            .iter()
            .filter(|(_, f)| now.duration_since(f.sent) >= ACK_TIMEOUT)
            .map(|(&s, _)| s)
            .collect();
        for sysid in timed_out {
            let Some(f) = self.inflight.remove(&sysid) else { continue };
            if f.item.attempts < MAX_ATTEMPTS {
                self.queues.entry(sysid).or_default().push_front(f.item);
            } else {
                self.reports.entry(sysid).or_default().gave_up += 1;
            }
        }

        let mut order: Vec<u8> = self.queues.iter().filter(|(_, q)| !q.is_empty()).map(|(&s, _)| s).collect();
        if let Some(c) = self.cursor {
            let split = order.partition_point(|&s| s <= c);
            order.rotate_left(split);
        }
        let mut out = Vec::new();
        for sysid in order {
            if self.tokens < FRAME_BYTES {
                break;
            }
            if self.inflight.contains_key(&sysid) {
                continue;
            }
            let Some(mut item) = self.queues.get_mut(&sysid).and_then(|q| q.pop_front()) else { continue };
            self.tokens -= FRAME_BYTES;
            item.attempts += 1;
            out.push((sysid, item.msg.clone()));
            self.cursor = Some(sysid);
            if item.kind == Kind::Rate {
                self.inflight.insert(sysid, Inflight { item, sent: now });
            }
        }

        let done: Vec<u8> = self
            .reports
            .keys()
            .copied()
            .filter(|s| self.queues.get(s).is_none_or(|q| q.is_empty()) && !self.inflight.contains_key(s))
            .collect();
        let finished = done.into_iter().filter_map(|s| self.reports.remove(&s).map(|r| (s, r))).collect();
        (out, finished)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::mavlink::ardupilotmega::{COMMAND_LONG_DATA, PARAM_REQUEST_READ_DATA};

    /// A SET_MESSAGE_INTERVAL tagged by its `param1` (the message id), so a test can tell sends apart.
    fn rate(n: f32) -> MavMessage {
        MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
            command: MavCmd::MAV_CMD_SET_MESSAGE_INTERVAL,
            param1: n,
            ..Default::default()
        })
    }

    fn one_shot() -> MavMessage {
        MavMessage::PARAM_REQUEST_READ(PARAM_REQUEST_READ_DATA::default())
    }

    fn param1(m: &MavMessage) -> f32 {
        match m {
            MavMessage::COMMAND_LONG(c) => c.param1,
            _ => -1.0,
        }
    }

    const ACK: MavCmd = MavCmd::MAV_CMD_SET_MESSAGE_INTERVAL;

    #[test]
    fn one_rate_command_in_flight_per_vehicle_until_its_ack() {
        let t = Instant::now();
        let mut q = SetupQueue::new(10_000.0);
        for n in 1..=3 {
            q.push(7, rate(n as f32), Kind::Rate);
        }
        let (out, _) = q.poll(t);
        assert_eq!(out.iter().map(|(_, m)| param1(m)).collect::<Vec<_>>(), [1.0]);
        assert!(q.poll(t).0.is_empty(), "the first is still unconfirmed");
        assert!(q.on_ack(7, ACK, MavResult::MAV_RESULT_ACCEPTED));
        assert_eq!(q.poll(t).0.iter().map(|(_, m)| param1(m)).collect::<Vec<_>>(), [2.0]);
    }

    #[test]
    fn vehicles_proceed_in_parallel_within_the_byte_budget() {
        let t = Instant::now();
        // 450 B/s, burst 225 B = 5 frames.
        let mut q = SetupQueue::new(450.0);
        for sysid in 1..=10 {
            q.push(sysid, one_shot(), Kind::OneShot);
        }
        assert_eq!(q.poll(t).0.len(), 5, "the initial burst");
        assert_eq!(q.poll(t).0.len(), 0, "no tokens left");
        assert_eq!(q.poll(t + Duration::from_millis(500)).0.len(), 5, "refilled at 450 B/s");
    }

    #[test]
    fn round_robin_serves_every_vehicle_before_repeating_one() {
        let t = Instant::now();
        let mut q = SetupQueue::new(45.0 * 4.0 * 2.0); // burst = 4 frames
        for sysid in [1, 2, 3, 4, 5, 6] {
            for _ in 0..3 {
                q.push(sysid, one_shot(), Kind::OneShot);
            }
        }
        let first: Vec<u8> = q.poll(t).0.into_iter().map(|(s, _)| s).collect();
        assert_eq!(first, [1, 2, 3, 4]);
        let next: Vec<u8> = q.poll(t + Duration::from_secs(1)).0.into_iter().map(|(s, _)| s).collect();
        assert_eq!(&next[..2], [5, 6], "the next poll starts after the last one served");
    }

    #[test]
    fn a_lost_rate_command_is_resent_then_given_up() {
        let t = Instant::now();
        let mut q = SetupQueue::new(10_000.0);
        q.push(3, rate(30.0), Kind::Rate);
        q.push(3, rate(33.0), Kind::Rate);
        let at = |k: u32| t + ACK_TIMEOUT * k;
        assert_eq!(param1(&q.poll(at(0)).0[0].1), 30.0);
        assert_eq!(param1(&q.poll(at(1)).0[0].1), 30.0, "second send");
        assert_eq!(param1(&q.poll(at(2)).0[0].1), 30.0, "third send");
        let (out, _) = q.poll(at(3));
        assert_eq!(param1(&out[0].1), 33.0, "given up after MAX_ATTEMPTS — the next one goes out");
        assert!(q.on_ack(3, ACK, MavResult::MAV_RESULT_ACCEPTED));
        let (_, finished) = q.poll(at(3));
        assert_eq!(finished, [(3, SetupReport { confirmed: 1, refused: 0, gave_up: 1 })]);
    }

    #[test]
    fn a_refusal_is_final() {
        let t = Instant::now();
        let mut q = SetupQueue::new(10_000.0);
        q.push(4, rate(231.0), Kind::Rate);
        q.poll(t);
        assert!(q.on_ack(4, ACK, MavResult::MAV_RESULT_DENIED));
        let (out, finished) = q.poll(t + ACK_TIMEOUT * 2);
        assert!(out.is_empty());
        assert_eq!(finished, [(4, SetupReport { confirmed: 0, refused: 1, gave_up: 0 })]);
    }

    #[test]
    fn other_acks_are_not_ours() {
        let t = Instant::now();
        let mut q = SetupQueue::new(10_000.0);
        assert!(!q.on_ack(5, ACK, MavResult::MAV_RESULT_ACCEPTED), "nothing in flight");
        q.push(5, rate(1.0), Kind::Rate);
        q.poll(t);
        assert!(!q.on_ack(5, MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH, MavResult::MAV_RESULT_ACCEPTED));
        assert!(!q.on_ack(6, ACK, MavResult::MAV_RESULT_ACCEPTED), "another vehicle's");
    }

    #[test]
    fn a_new_profile_supersedes_queued_rate_commands_only() {
        let t = Instant::now();
        let mut q = SetupQueue::new(10_000.0);
        q.push(8, rate(1.0), Kind::Rate);
        q.push(8, rate(2.0), Kind::Rate);
        q.push(8, one_shot(), Kind::OneShot);
        assert_eq!(param1(&q.poll(t).0[0].1), 1.0);
        q.replace_rates(8, vec![rate(9.0)]);
        assert!(q.on_ack(8, ACK, MavResult::MAV_RESULT_ACCEPTED));
        let sent: Vec<f32> = (0..3)
            .flat_map(|k| {
                let out = q.poll(t + Duration::from_millis(10 * k)).0;
                let _ = q.on_ack(8, ACK, MavResult::MAV_RESULT_ACCEPTED);
                out.into_iter().map(|(_, m)| param1(&m)).collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(sent, [-1.0, 9.0], "the one-shot stays; rate 2 was superseded by 9");
    }

    #[test]
    fn a_forgotten_vehicle_sends_nothing_more() {
        let t = Instant::now();
        let mut q = SetupQueue::new(10_000.0);
        q.push(9, rate(1.0), Kind::Rate);
        q.push(9, rate(2.0), Kind::Rate);
        q.poll(t);
        q.forget(9);
        let (out, finished) = q.poll(t + ACK_TIMEOUT);
        assert!(out.is_empty() && finished.is_empty());
    }
}
