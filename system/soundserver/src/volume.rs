//! The `volume` port: the master level and mute, on the codec.
//!
//! The compositor holds the one connector. Each request goes to the output
//! driver played to, which sets it on the codec and reads it back, and the
//! answer is what the codec read back, never the intent, sent by one write
//! that does not wait: a peer that will not take it is dropped by name. A
//! device with no master — virtio-sound, the null sink, a path with no
//! amplifier to set — answers every request unavailable, by name, and no
//! mixer gain stands in for a master.

use toyos::audio::{Volume, VolumeStep, MSG_VOLUME, MSG_VOLUME_MUTE, MSG_VOLUME_STEP, MSG_VOLUME_UNAVAILABLE};
use toyos::ipc::{self, RxStep};
use toyos::Connection;

use toyos_sound_output::Master;

use crate::output::{Asked, Outputs};

pub(crate) struct VolumeClient {
    pub(crate) conn: Connection,
    rx: ipc::FrameRx<{ core::mem::size_of::<VolumeStep>() }>,
    /// Which connection this is, for [`Relay`]: a reconnect is another.
    id: u64,
}

/// The one master request forwarded to the output driver and not yet
/// answered, and whose it is.
///
/// **One at a time**: a holder's next request is not read until its last is
/// answered, so a holder writing faster than the codec answers fills its own
/// connection and never the driver's. **And an answer goes to the connection
/// that asked**: one whose asker reconnected since is dropped.
#[derive(Default)]
pub(crate) struct Relay {
    in_flight: Option<u64>,
    next_id: u64,
}

impl Relay {
    /// Whether a request may be read now.
    pub(crate) fn idle(&self) -> bool {
        self.in_flight.is_none()
    }

    fn forwarded(&mut self, by: u64) {
        assert!(self.in_flight.replace(by).is_none(), "soundserver: two master requests in flight");
    }

    /// The asker the answer that just came is owed to, if one is in flight.
    fn answered(&mut self) -> Option<u64> {
        self.in_flight.take()
    }

    fn mint(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
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
pub(crate) fn accepted(clients: &mut Vec<VolumeClient>, relay: &mut Relay, conn: Connection) {
    clients.clear();
    clients.push(VolumeClient { conn, rx: ipc::FrameRx::new(), id: relay.mint() });
}

/// Serve the requests the client has framed, up to one the output driver
/// answers, which [`answer`] passes on; `false` once it is to go.
pub(crate) fn serve(client: &mut VolumeClient, outputs: &mut Outputs, relay: &mut Relay) -> bool {
    while relay.idle() {
        let (msg_type, payload_len) = match client.rx.pump(&client.conn) {
            RxStep::Idle => return true,
            RxStep::Eof => return false,
            RxStep::Malformed => {
                say!("soundserver: a volume frame this protocol cannot describe, disconnecting");
                return false;
            }
            RxStep::Frame { msg_type, payload_len, .. } => (msg_type, payload_len),
        };
        let asked = match classify(msg_type, payload_len) {
            Request::Step => {
                let step: VolumeStep = ipc::decode_payload(client.rx.payload(payload_len))
                    .expect("a payload as long as the struct decodes");
                outputs.ask(Some(step.delta))
            }
            Request::Mute => outputs.ask(None),
            Request::Violation => {
                say!("soundserver: volume protocol violation (msg {msg_type}), disconnecting");
                return false;
            }
        };
        let sent = match asked {
            Asked::Forwarded => {
                relay.forwarded(client.id);
                Ok(())
            }
            Asked::Unavailable => send(&client.conn, None),
            Asked::Refused(why) => {
                say!("soundserver: a master request answered unavailable: {why}");
                send(&client.conn, None)
            }
        };
        if let Err(e) = sent {
            say!("soundserver: a volume answer was not taken ({e:?}), disconnecting");
            return false;
        }
    }
    true
}

/// The output driver's answer to the request in flight, to the connection
/// that asked it, where that is still the holder; a holder that will not take
/// it goes.
pub(crate) fn answer(clients: &mut Vec<VolumeClient>, relay: &mut Relay, master: Option<Master>) {
    let Some(asker) = relay.answered() else {
        say!("soundserver: the output driver answered a master request nobody made");
        return;
    };
    let Some(client) = clients.first().filter(|client| client.id == asker) else { return };
    if let Err(e) = send(&client.conn, master) {
        say!("soundserver: a volume answer was not taken ({e:?}), disconnecting");
        clients.clear();
    }
}

fn send(conn: &Connection, answer: Option<Master>) -> Result<(), ipc::TrySendError> {
    match answer {
        Some(Master { position, bottom, muted }) => {
            conn.try_send(MSG_VOLUME, &Volume { position, bottom, muted: muted as u8, _pad: 0 })
        }
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
