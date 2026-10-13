//! The `volume` port: the master level and mute, on the codec.
//!
//! The compositor holds the one connector. Each request is set on the device
//! and read back, and the answer is what the device read back, never the
//! intent, sent by one write that does not wait: a peer that will not take it
//! is dropped by name. A machine whose device has no master — virtio-sound,
//! the null sink, an HDA path with no amplifier to set — answers every
//! request unavailable, by name, and no mixer gain stands in for a master.

use toyos::audio::{Volume, VolumeStep, MSG_VOLUME, MSG_VOLUME_MUTE, MSG_VOLUME_STEP, MSG_VOLUME_UNAVAILABLE};
use toyos::ipc::{self, RxStep};
use toyos::Connection;

use crate::hda::{Answer, MasterControl};

/// The widest request decoded, plus one, so an over-long frame is refused
/// rather than truncated into a request.
const MAX_KEPT_PAYLOAD: usize = core::mem::size_of::<VolumeStep>() + 1;

pub(crate) struct VolumeClient {
    pub(crate) conn: Connection,
    rx: ipc::FrameRx<MAX_KEPT_PAYLOAD>,
}

/// What one framed request asks for, decided before anything is done.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Request {
    Step,
    Mute,
    Violation,
}

fn classify(msg_type: u32, payload_len: usize) -> Request {
    match (msg_type, payload_len) {
        (MSG_VOLUME_STEP, len) if len == core::mem::size_of::<VolumeStep>() => Request::Step,
        (MSG_VOLUME_MUTE, 0) => Request::Mute,
        _ => Request::Violation,
    }
}

/// A new connection: one holder at a time, so one before it is a holder that
/// reconnected, and it goes.
pub(crate) fn accepted(clients: &mut Vec<VolumeClient>, conn: Connection) {
    clients.clear();
    clients.push(VolumeClient { conn, rx: ipc::FrameRx::new() });
}

/// Serve every request the client has framed; `false` once it is to go.
pub(crate) fn serve(client: &mut VolumeClient, mut master: Option<&mut MasterControl>) -> bool {
    loop {
        let (msg_type, payload_len) = match client.rx.pump(&client.conn) {
            RxStep::Idle => return true,
            RxStep::Eof => return false,
            RxStep::Malformed => {
                say!("soundserver: a volume frame this protocol cannot describe, disconnecting");
                return false;
            }
            RxStep::Frame { msg_type, payload_len } => (msg_type, payload_len),
        };
        let answer = match classify(msg_type, payload_len) {
            Request::Step => {
                let step: VolumeStep = ipc::decode_payload(client.rx.payload(payload_len))
                    .expect("a payload as long as the struct decodes");
                master.as_deref_mut().and_then(|master| master.step(step.delta))
            }
            Request::Mute => master.as_deref_mut().and_then(MasterControl::toggle_mute),
            Request::Violation => {
                say!("soundserver: volume protocol violation (msg {msg_type}), disconnecting");
                return false;
            }
        };
        if let Err(e) = send(&client.conn, answer) {
            say!("soundserver: a volume answer was not taken ({e:?}), disconnecting");
            return false;
        }
    }
}

fn send(conn: &Connection, answer: Option<Answer>) -> Result<(), ipc::TrySendError> {
    match answer {
        Some(Answer { setting, bottom }) => conn.try_send(
            MSG_VOLUME,
            &Volume { position: setting.position, bottom, muted: setting.muted as u8, _pad: 0 },
        ),
        None => conn.try_send_bytes(MSG_VOLUME_UNAVAILABLE, &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_volume_request_is_refused_unless_its_type_and_width_fit() {
        const STEP: usize = core::mem::size_of::<VolumeStep>();
        assert_eq!(classify(MSG_VOLUME_STEP, STEP), Request::Step);
        assert_eq!(classify(MSG_VOLUME_MUTE, 0), Request::Mute);
        for wrong in [0, STEP - 1, STEP + 1] {
            assert_eq!(classify(MSG_VOLUME_STEP, wrong), Request::Violation);
        }
        assert_eq!(classify(MSG_VOLUME_MUTE, 1), Request::Violation);
        // The two soundserver only ever sends, and anything else.
        for other in [0, MSG_VOLUME, MSG_VOLUME_UNAVAILABLE, u32::MAX] {
            for len in [0, STEP] {
                assert_eq!(classify(other, len), Request::Violation);
            }
        }
    }
}
