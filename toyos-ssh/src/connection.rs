//! The connection layer of an authenticated session (RFC 4254): `session`
//! channels keyed by the id the server gave each, every one serving one
//! `exec`, and the windows that bound what each side may send.
//!
//! **A [`ChannelId`] exists only here**, made when a channel opens, so a
//! channel event cannot be written outside an authenticated [`Session`]. It is
//! the server's number for the channel, and no number is given twice in a
//! session, so an id the driver still holds after [`Event::Closed`] is
//! [`Gone`] and never names a channel opened later. A message naming a channel
//! the session does not hold ends the session. A channel the server has closed
//! tells the driver nothing more. Every
//! other channel type, every other channel request and every global request is
//! refused by name.
//!
//! **The client's window is spent as the driver consumes**: the server returns
//! window only for bytes the driver says it has taken, so a program that reads
//! nothing stops its own channel and no other.

use crate::auth::Verified;
use crate::wire::{put_bool, put_string, put_u32, Reader, Refusal};
use crate::{msg, Declined, Event, Gone};

/// Channels open at once: OpenSSH's `MaxSessions`.
const MAX_CHANNELS: usize = 10;

/// The window the client is given, and given back as it is consumed.
const WINDOW: u32 = 2 * 1024 * 1024;

/// The longest data packet either side sends.
const MAX_PACKET: u32 = 32 * 1024;

const COMMAND_CAP: usize = 32 * 1024;

const OPEN_ADMINISTRATIVELY_PROHIBITED: u32 = 1;
const OPEN_RESOURCE_SHORTAGE: u32 = 4;

/// A channel of this session, named by the server's own number for it, which
/// names no other channel of the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelId(u32);

struct Channel {
    /// The server's number for it.
    number: u32,
    /// The client's number for it.
    peer: u32,
    /// What the server may still send, and in packets of at most `send_max`.
    send_window: u32,
    send_max: u32,
    /// What the client may still send.
    recv_window: u32,
    /// Bytes handed to the driver it has not consumed.
    unconsumed: u32,
    /// Bytes consumed and not yet given back as window.
    consumed: u32,
    command: bool,
    eof: bool,
    /// The server sent CLOSE and awaits the client's.
    closing: bool,
}

/// An authenticated session's channels.
pub(crate) struct Session {
    channels: [Option<Channel>; MAX_CHANNELS],
    /// The number the next channel opened is given.
    next: Option<u32>,
}

impl Session {
    /// The connection layer, which only a verified signature opens.
    pub(crate) fn new(_proof: Verified) -> Self {
        Self { channels: Default::default(), next: Some(0) }
    }

    /// One message of the connection layer.
    pub(crate) fn handle(&mut self, payload: &[u8], out: &mut Vec<Vec<u8>>, events: &mut Vec<Event>) -> Result<(), Refusal> {
        let mut r = Reader::new(payload);
        match r.byte("message")? {
            // Ignored once authentication succeeded (RFC 4252 §5.1).
            msg::USERAUTH_REQUEST => Ok(()),
            msg::GLOBAL_REQUEST => {
                let name = r.name("request name")?;
                let want_reply = r.boolean("want reply")?;
                events.push(Event::Declined(Declined::GlobalRequest(name.to_string())));
                if want_reply {
                    out.push(vec![msg::REQUEST_FAILURE]);
                }
                Ok(())
            }
            msg::CHANNEL_OPEN => self.open(r, out, events),
            message @ (msg::CHANNEL_WINDOW_ADJUST
            | msg::CHANNEL_DATA
            | msg::CHANNEL_EOF
            | msg::CHANNEL_CLOSE
            | msg::CHANNEL_REQUEST) => {
                let id = ChannelId(r.u32("recipient channel")?);
                let (slot, channel) = self
                    .channels
                    .iter_mut()
                    .enumerate()
                    .find_map(|(slot, c)| c.as_mut().filter(|c| c.number == id.0).map(|c| (slot, c)))
                    .ok_or(Refusal::Malformed("a channel this session does not hold"))?;
                match message {
                    msg::CHANNEL_WINDOW_ADJUST => {
                        let bytes = r.u32("bytes to add")?;
                        r.end("CHANNEL_WINDOW_ADJUST")?;
                        channel.send_window =
                            channel.send_window.checked_add(bytes).ok_or(Refusal::Malformed("a window past 2^32 - 1 bytes"))?;
                        if !channel.closing {
                            events.push(Event::Writable { channel: id });
                        }
                    }
                    msg::CHANNEL_DATA => {
                        let data = r.string("data", MAX_PACKET as usize)?;
                        r.end("CHANNEL_DATA")?;
                        if channel.eof {
                            return Err(Refusal::Malformed("data after the client's EOF"));
                        }
                        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
                        channel.recv_window =
                            channel.recv_window.checked_sub(len).ok_or(Refusal::Malformed("data beyond the window"))?;
                        if !channel.closing {
                            channel.unconsumed += len;
                            events.push(Event::Data { channel: id, data: data.to_vec() });
                        }
                    }
                    msg::CHANNEL_EOF => {
                        r.end("CHANNEL_EOF")?;
                        channel.eof = true;
                        if !channel.closing {
                            events.push(Event::Eof { channel: id });
                        }
                    }
                    msg::CHANNEL_CLOSE => {
                        r.end("CHANNEL_CLOSE")?;
                        if !channel.closing {
                            out.push(to_peer(msg::CHANNEL_CLOSE, channel.peer));
                            events.push(Event::Closed { channel: id });
                        }
                        self.channels[slot] = None;
                    }
                    _ => {
                        let request = r.name("request type")?;
                        let want_reply = r.boolean("want reply")?;
                        if channel.closing {
                            return Ok(());
                        }
                        let reply = if request == "exec" && !channel.command {
                            let command = r.string("command", COMMAND_CAP)?;
                            r.end("CHANNEL_REQUEST exec")?;
                            channel.command = true;
                            events.push(Event::Exec { channel: id, command: command.to_vec() });
                            msg::CHANNEL_SUCCESS
                        } else {
                            events.push(Event::Declined(Declined::Request(request.to_string())));
                            msg::CHANNEL_FAILURE
                        };
                        if want_reply {
                            out.push(to_peer(reply, channel.peer));
                        }
                    }
                }
                Ok(())
            }
            message => Err(Refusal::Unexpected { phase: "the session", message }),
        }
    }

    fn open(&mut self, mut r: Reader<'_>, out: &mut Vec<Vec<u8>>, events: &mut Vec<Event>) -> Result<(), Refusal> {
        let kind = r.name("channel type")?;
        let peer = r.u32("sender channel")?;
        let window = r.u32("initial window size")?;
        let max = r.u32("maximum packet size")?;
        let refuse = |reason, why: &str| {
            let mut failure = to_peer(msg::CHANNEL_OPEN_FAILURE, peer);
            put_u32(&mut failure, reason);
            put_string(&mut failure, why.as_bytes());
            put_string(&mut failure, b"");
            failure
        };
        if kind != "session" {
            events.push(Event::Declined(Declined::ChannelType(kind.to_string())));
            out.push(refuse(OPEN_ADMINISTRATIVELY_PROHIBITED, "only session channels are served"));
            return Ok(());
        }
        r.end("CHANNEL_OPEN session")?;
        let (Some(slot), Some(number)) = (self.channels.iter().position(Option::is_none), self.next) else {
            events.push(Event::Declined(Declined::ChannelLimit));
            out.push(refuse(OPEN_RESOURCE_SHORTAGE, "every channel this session may hold is open"));
            return Ok(());
        };
        self.next = number.checked_add(1);
        self.channels[slot] = Some(Channel {
            number,
            peer,
            send_window: window,
            send_max: max.min(MAX_PACKET),
            recv_window: WINDOW,
            unconsumed: 0,
            consumed: 0,
            command: false,
            eof: false,
            closing: false,
        });
        let mut confirmation = to_peer(msg::CHANNEL_OPEN_CONFIRMATION, peer);
        put_u32(&mut confirmation, number);
        put_u32(&mut confirmation, WINDOW);
        put_u32(&mut confirmation, MAX_PACKET);
        out.push(confirmation);
        Ok(())
    }

    fn live(&mut self, id: ChannelId) -> Result<&mut Channel, Gone> {
        self.channels.iter_mut().flatten().find(|c| c.number == id.0 && !c.closing).ok_or(Gone)
    }

    /// Send as much of `data` as the client's window takes, as standard output
    /// or, `stderr`, as extended data (RFC 4254 §5.2). Returns how much.
    pub(crate) fn send(&mut self, id: ChannelId, data: &[u8], stderr: bool, out: &mut Vec<Vec<u8>>) -> Result<usize, Gone> {
        let channel = self.live(id)?;
        let mut sent = 0;
        while sent < data.len() {
            let n = (data.len() - sent).min(channel.send_window as usize).min(channel.send_max as usize);
            if n == 0 {
                break;
            }
            let mut packet = to_peer(if stderr { msg::CHANNEL_EXTENDED_DATA } else { msg::CHANNEL_DATA }, channel.peer);
            if stderr {
                put_u32(&mut packet, 1);
            }
            put_string(&mut packet, &data[sent..sent + n]);
            out.push(packet);
            channel.send_window -= n as u32;
            sent += n;
        }
        Ok(sent)
    }

    /// The program's exit status, then EOF and CLOSE (RFC 4254 §6.10).
    pub(crate) fn exit(&mut self, id: ChannelId, status: u32, out: &mut Vec<Vec<u8>>) -> Result<(), Gone> {
        let channel = self.live(id)?;
        let mut request = to_peer(msg::CHANNEL_REQUEST, channel.peer);
        put_string(&mut request, b"exit-status");
        put_bool(&mut request, false);
        put_u32(&mut request, status);
        out.push(request);
        out.push(to_peer(msg::CHANNEL_EOF, channel.peer));
        out.push(to_peer(msg::CHANNEL_CLOSE, channel.peer));
        channel.closing = true;
        Ok(())
    }

    /// The driver took `n` more bytes of the channel's data; give the window
    /// back once half of it is spent.
    pub(crate) fn consumed(&mut self, id: ChannelId, n: usize, out: &mut Vec<Vec<u8>>) -> Result<(), Gone> {
        let channel = self.live(id)?;
        let n = u32::try_from(n).ok().filter(|&n| n <= channel.unconsumed).expect("the driver consumes no more than it was given");
        channel.unconsumed -= n;
        channel.consumed += n;
        if channel.consumed >= WINDOW / 2 {
            let mut adjust = to_peer(msg::CHANNEL_WINDOW_ADJUST, channel.peer);
            put_u32(&mut adjust, channel.consumed);
            out.push(adjust);
            channel.recv_window += channel.consumed;
            channel.consumed = 0;
        }
        Ok(())
    }
}

/// A channel message's first two fields.
fn to_peer(message: u8, peer: u32) -> Vec<u8> {
    let mut out = vec![message];
    put_u32(&mut out, peer);
    out
}
