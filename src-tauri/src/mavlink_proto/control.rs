// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Marc Hoffmann (b14ckyy)

// MAVLink Vehicle-Control Commands
// Fire a COMMAND_LONG / COMMAND_INT to the FC and wait for its COMMAND_ACK.
// Ref: https://mavlink.io/en/services/command.html
//
// Like the mission microprotocol (mission.rs), this works entirely through the MavlinkCommand
// channel — it never holds the AppState mutex during the exchange, so disconnect stays safe.
//
// MAVLink commands are NOT request→response like MSP: we send the command and the FC replies
// asynchronously with COMMAND_ACK (ACCEPTED / DENIED / TEMPORARILY_REJECTED / UNSUPPORTED / …).
// We register a receiver, send, then match the ACK by command id and resolve the blocking call.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use ::mavlink::ardupilotmega::{
    MavMessage, MavCmd, MavFrame, MavResult, MavParamType,
    COMMAND_LONG_DATA, COMMAND_INT_DATA, PARAM_SET_DATA,
};

use super::handler::MavlinkCommand;

/// The autopilot is conventionally component 1 (MAV_COMP_ID_AUTOPILOT1). ArduPilot and PX4 both
/// answer commands addressed to it.
const AUTOPILOT_COMPONENT: u8 = 1;

/// How long we wait for the COMMAND_ACK before giving up. Commands are cheap; the FC acks fast.
const ACK_TIMEOUT: Duration = Duration::from_secs(3);

/// Retransmission (MAVLink command protocol): on a lossy link (a radio mesh) the command or its ACK
/// can be lost, and one lost datagram used to fail the whole command after ACK_TIMEOUT — an RTL to a
/// fleet left some aircraft unreached. Resend after `interval` without an ACK, up to `attempts` sends;
/// COMMAND_LONG counts the resend in `confirmation`. Any final ACK (accepted or not) ends it; an
/// IN_PROGRESS ack stops resending and waits up to `final_wait` for the result. The worst case stays
/// ACK_TIMEOUT (3 × 1 s), as before.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryPolicy {
    pub interval: Duration,
    pub attempts: u8,
    pub final_wait: Duration,
}

pub(crate) const COMMAND_RETRY: RetryPolicy =
    RetryPolicy { interval: Duration::from_secs(1), attempts: 3, final_wait: ACK_TIMEOUT };

/// Send a `COMMAND_LONG` and wait for its `COMMAND_ACK`. `params` are param1..param7.
pub fn send_command_long(
    cmd_tx: &mpsc::Sender<MavlinkCommand>,
    fc_sysid: u8,
    command: MavCmd,
    params: [f32; 7],
) -> Result<(), String> {
    command_with_retries(cmd_tx, fc_sysid, command, COMMAND_RETRY, |attempt| {
        MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
            target_system: fc_sysid,
            target_component: AUTOPILOT_COMPONENT,
            command,
            confirmation: attempt,
            param1: params[0],
            param2: params[1],
            param3: params[2],
            param4: params[3],
            param5: params[4],
            param6: params[5],
            param7: params[6],
        })
    })
}

/// Send a `COMMAND_INT` and wait for its `COMMAND_ACK`. Used for commands carrying a global
/// position (lat/lon as int32 × 1e7) — `DO_REPOSITION` — where COMMAND_LONG's f32 params would
/// lose coordinate precision. `params` are param1..param4; `x`/`y` are lat/lon × 1e7; `z` is alt (m).
#[allow(clippy::too_many_arguments)] // maps directly to COMMAND_INT's fixed field set
pub fn send_command_int(
    cmd_tx: &mpsc::Sender<MavlinkCommand>,
    fc_sysid: u8,
    frame: MavFrame,
    command: MavCmd,
    params: [f32; 4],
    x: i32,
    y: i32,
    z: f32,
) -> Result<(), String> {
    // COMMAND_INT has no `confirmation` field: a resend is identical.
    command_with_retries(cmd_tx, fc_sysid, command, COMMAND_RETRY, |_attempt| {
        MavMessage::COMMAND_INT(COMMAND_INT_DATA {
            target_system: fc_sysid,
            target_component: AUTOPILOT_COMPONENT,
            frame,
            command,
            current: 0,
            autocontinue: 0,
            param1: params[0],
            param2: params[1],
            param3: params[2],
            param4: params[3],
            x,
            y,
            z,
        })
    })
}

/// Register for `sysid`'s ACKs, send `build(attempt)` until a final ACK arrives (see `RetryPolicy`),
/// unregister. `attempt` is 0 for the first send.
pub(crate) fn command_with_retries(
    cmd_tx: &mpsc::Sender<MavlinkCommand>,
    sysid: u8,
    command: MavCmd,
    policy: RetryPolicy,
    mut build: impl FnMut(u8) -> MavMessage,
) -> Result<(), String> {
    let rx = register(cmd_tx, sysid)?;
    let result = (|| {
        for attempt in 0..policy.attempts {
            if attempt > 0 {
                log::debug!("MAVLink {:?} → sysid={}: no ACK, resend #{}", command, sysid, attempt);
            }
            send(cmd_tx, build(attempt))?;
            match wait_for_ack(&rx, command, policy.interval)? {
                AckWait::Final(r) => {
                    if attempt > 0 {
                        log::info!("MAVLink {:?} → sysid={}: ACK after {} resend(s)", command, sysid, attempt);
                    }
                    return r;
                }
                AckWait::InProgress => return wait_final(&rx, command, policy.final_wait),
                AckWait::Silent => {}
            }
        }
        log::warn!("MAVLink {:?} → sysid={}: no COMMAND_ACK after {} send(s)", command, sysid, policy.attempts);
        Err("No COMMAND_ACK from FC (timed out)".to_string())
    })();
    unregister(cmd_tx, sysid);
    result
}

/// Set a single FC parameter. Used for tunables that have no dedicated command — e.g. the fixed-wing
/// loiter radius (`WP_LOITER_RAD`). ArduPilot ignores `param_type` for its REAL32 params, so there the
/// `PARAM_SET` goes out fire-and-forget; on PX4 the parameter is READ first (`PARAM_REQUEST_READ`, the
/// reply carries the declared type; up to `params_rt::PARAM_TIMEOUT`) and an unknown or silent
/// parameter is an `Err`. Neither path waits for the PARAM_VALUE echo of the write (non-critical, keeps
/// it simple).
pub fn set_param(
    cmd_tx: &mpsc::Sender<MavlinkCommand>,
    fc_sysid: u8,
    name: &str,
    value: f32,
    px4: bool,
) -> Result<(), String> {
    let mut param_id = [0u8; 16];
    let bytes = name.as_bytes();
    let n = bytes.len().min(16);
    param_id[..n].copy_from_slice(&bytes[..n]);

    // ArduPilot takes any parameter as a REAL32 number. PX4 insists on the parameter's declared type
    // AND byte-casts integers into the float field ("param types mismatch" otherwise) — so on PX4 read
    // the type first and encode accordingly.
    let (param_value, param_type) = if px4 {
        match super::params_rt::read_params_typed(cmd_tx, fc_sysid, &[name]).get(name) {
            Some((_, ty)) => (super::params_rt::encode_param_bytecast(value, *ty), *ty),
            None => return Err(format!("Parameter {name} not reported by the FC")),
        }
    } else {
        (value, MavParamType::MAV_PARAM_TYPE_REAL32)
    };

    send(cmd_tx, MavMessage::PARAM_SET(PARAM_SET_DATA {
        target_system: fc_sysid,
        target_component: AUTOPILOT_COMPONENT,
        param_id: param_id.into(),
        param_value,
        param_type,
    }))
}

/// Send an explicit RC_CHANNELS_OVERRIDE **release** frame for the channels we were controlling
/// (`controlled_us`: positional µs, non-zero = controlled), repeated `count` times at `interval`, to
/// hand control back on a *deliberate* disengage (ArduPilot). Per the MAVLink field semantics a value of
/// 0 (CH1–8) / 65534 (CH9–18) releases that channel back to the RC radio; channels we never touched stay
/// ignored. Fire-and-forget (RC stream messages have no ACK). The caller must first disable the normal
/// RC stream so the handler doesn't interleave live overrides with these release frames.
pub fn send_rc_release(
    cmd_tx: &mpsc::Sender<MavlinkCommand>,
    fc_sysid: u8,
    controlled_us: &[u16],
    count: u8,
    interval: Duration,
) -> Result<(), String> {
    let ch = crate::scheduler::rc_tx::release_channels(controlled_us);
    let msg = super::handler::rc_override_msg(fc_sysid, &ch);
    for i in 0..count {
        send(cmd_tx, msg.clone())?;
        if i + 1 < count {
            std::thread::sleep(interval);
        }
    }
    Ok(())
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn register(cmd_tx: &mpsc::Sender<MavlinkCommand>, sysid: u8) -> Result<mpsc::Receiver<MavMessage>, String> {
    let (tx, rx) = mpsc::channel();
    cmd_tx.send(MavlinkCommand::RegisterCommandReceiver { sysid, tx })
        .map_err(|_| "MAVLink handler stopped".to_string())?;
    // Same tiny race guard as mission.rs: let the handler pick up the registration before we send.
    std::thread::sleep(Duration::from_millis(10));
    Ok(rx)
}

fn unregister(cmd_tx: &mpsc::Sender<MavlinkCommand>, sysid: u8) {
    let _ = cmd_tx.send(MavlinkCommand::UnregisterCommandReceiver { sysid });
}

fn send(cmd_tx: &mpsc::Sender<MavlinkCommand>, msg: MavMessage) -> Result<(), String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    cmd_tx.send(MavlinkCommand::SendMessage { msg, reply: reply_tx })
        .map_err(|_| "MAVLink handler stopped".to_string())?;
    reply_rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| "MAVLink send timed out".to_string())?
}

/// What one wait window produced.
#[derive(Debug, PartialEq)]
enum AckWait {
    /// A final ACK for the command (accepted, or the FC's refusal as the error text).
    Final(Result<(), String>),
    /// The FC acknowledged it is working on it — stop resending.
    InProgress,
    /// Nothing for this command inside the window.
    Silent,
}

/// The outcome of a final ACK result; `None` for IN_PROGRESS (not final).
fn ack_outcome(result: MavResult) -> Option<Result<(), String>> {
    Some(match result {
        MavResult::MAV_RESULT_ACCEPTED => Ok(()),
        MavResult::MAV_RESULT_IN_PROGRESS => return None,
        MavResult::MAV_RESULT_TEMPORARILY_REJECTED => Err("Command temporarily rejected — try again".into()),
        MavResult::MAV_RESULT_DENIED => Err("Command denied by the flight controller".into()),
        MavResult::MAV_RESULT_UNSUPPORTED => Err("Command not supported by this firmware".into()),
        MavResult::MAV_RESULT_FAILED => Err("Command failed on the flight controller".into()),
        other => Err(format!("Command rejected: {:?}", other)),
    })
}

/// Wait up to `timeout` for an ACK matching `command`. `Err` only when the handler is gone.
fn wait_for_ack(rx: &mpsc::Receiver<MavMessage>, command: MavCmd, timeout: Duration) -> Result<AckWait, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(AckWait::Silent);
        }
        match rx.recv_timeout(remaining) {
            Ok(MavMessage::COMMAND_ACK(ack)) if ack.command == command => {
                return Ok(match ack_outcome(ack.result) {
                    Some(r) => AckWait::Final(r),
                    None => AckWait::InProgress,
                });
            }
            Ok(_) => continue, // ack for a different command, or stray msg — keep waiting
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(AckWait::Silent),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("MAVLink handler stopped".into()),
        }
    }
}

/// After IN_PROGRESS: wait for the final result (further IN_PROGRESS acks keep waiting).
fn wait_final(rx: &mpsc::Receiver<MavMessage>, command: MavCmd, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        match wait_for_ack(rx, command, deadline.saturating_duration_since(Instant::now()))? {
            AckWait::Final(r) => return r,
            AckWait::InProgress => continue,
            AckWait::Silent => return Err("No COMMAND_ACK from FC (timed out)".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::mavlink::ardupilotmega::COMMAND_ACK_DATA;

    const FAST: RetryPolicy =
        RetryPolicy { interval: Duration::from_millis(60), attempts: 3, final_wait: Duration::from_millis(300) };
    const CMD: MavCmd = MavCmd::MAV_CMD_DO_SET_MODE;

    fn ack(result: MavResult) -> MavMessage {
        MavMessage::COMMAND_ACK(COMMAND_ACK_DATA { command: CMD, result, ..Default::default() })
    }

    /// Stand-in for the handler thread: records the `confirmation` of every COMMAND_LONG sent and, on
    /// the send numbered `ack_on` (0-based), answers with `acks` in order. Returns the recorded sends.
    fn run(ack_on: Option<usize>, acks: Vec<MavMessage>) -> (Result<(), String>, Vec<u8>) {
        let (tx, rx) = mpsc::channel::<MavlinkCommand>();
        let handler = std::thread::spawn(move || {
            let mut ack_tx: Option<mpsc::Sender<MavMessage>> = None;
            let mut sends = Vec::new();
            while let Ok(cmd) = rx.recv() {
                match cmd {
                    MavlinkCommand::RegisterCommandReceiver { sysid, tx } => {
                        assert_eq!(sysid, 7);
                        ack_tx = Some(tx);
                    }
                    MavlinkCommand::UnregisterCommandReceiver { sysid } => {
                        assert_eq!(sysid, 7);
                        break;
                    }
                    MavlinkCommand::SendMessage { msg, reply } => {
                        if let MavMessage::COMMAND_LONG(c) = &msg {
                            assert_eq!(c.target_system, 7);
                            sends.push(c.confirmation);
                        }
                        let _ = reply.send(Ok(()));
                        if ack_on == Some(sends.len() - 1) {
                            for a in &acks {
                                let _ = ack_tx.as_ref().expect("registered before send").send(a.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
            sends
        });
        let result = command_with_retries(&tx, 7, CMD, FAST, |attempt| {
            MavMessage::COMMAND_LONG(COMMAND_LONG_DATA {
                target_system: 7,
                command: CMD,
                confirmation: attempt,
                ..Default::default()
            })
        });
        (result, handler.join().expect("fake handler"))
    }

    #[test]
    fn first_ack_ends_it_without_resend() {
        let (r, sends) = run(Some(0), vec![ack(MavResult::MAV_RESULT_ACCEPTED)]);
        assert_eq!(r, Ok(()));
        assert_eq!(sends, vec![0]);
    }

    #[test]
    fn lost_sends_are_retried_with_rising_confirmation() {
        let (r, sends) = run(Some(2), vec![ack(MavResult::MAV_RESULT_ACCEPTED)]);
        assert_eq!(r, Ok(()));
        assert_eq!(sends, vec![0, 1, 2]);
    }

    #[test]
    fn no_ack_at_all_times_out_after_every_attempt() {
        let (r, sends) = run(None, vec![]);
        assert_eq!(r, Err("No COMMAND_ACK from FC (timed out)".to_string()));
        assert_eq!(sends, vec![0, 1, 2]);
    }

    #[test]
    fn a_refusal_is_final_and_not_retried() {
        let (r, sends) = run(Some(0), vec![ack(MavResult::MAV_RESULT_DENIED)]);
        assert_eq!(r, Err("Command denied by the flight controller".to_string()));
        assert_eq!(sends, vec![0]);
    }

    #[test]
    fn in_progress_stops_resending_and_waits_for_the_result() {
        let (r, sends) =
            run(Some(0), vec![ack(MavResult::MAV_RESULT_IN_PROGRESS), ack(MavResult::MAV_RESULT_ACCEPTED)]);
        assert_eq!(r, Ok(()));
        assert_eq!(sends, vec![0]);
    }

    #[test]
    fn in_progress_without_a_result_times_out_without_resending() {
        let (r, sends) = run(Some(0), vec![ack(MavResult::MAV_RESULT_IN_PROGRESS)]);
        assert_eq!(r, Err("No COMMAND_ACK from FC (timed out)".to_string()));
        assert_eq!(sends, vec![0]);
    }

    #[test]
    fn an_ack_for_another_command_is_ignored() {
        let other = MavMessage::COMMAND_ACK(COMMAND_ACK_DATA {
            command: MavCmd::MAV_CMD_COMPONENT_ARM_DISARM,
            result: MavResult::MAV_RESULT_ACCEPTED,
            ..Default::default()
        });
        let (r, sends) = run(Some(1), vec![other, ack(MavResult::MAV_RESULT_ACCEPTED)]);
        assert_eq!(r, Ok(()));
        assert_eq!(sends, vec![0, 1]);
    }
}
