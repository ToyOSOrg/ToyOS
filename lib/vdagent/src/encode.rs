//! The agent's messages, written byte for byte as `vd_agent.h`'s packed
//! layouts, each chunked at [`MAX_CHUNK`] for `VDP_CLIENT_PORT`.
//!
//! No selection prefix: the agent announces `CLIPBOARD_BY_DEMAND` and
//! `CLIPBOARD_GRAB_SERIAL` and never `CLIPBOARD_SELECTION`, so every `GRAB`
//! carries its serial.

use crate::stream::MAX_CHUNK;
use crate::wire;

/// The capabilities the agent announces.
pub const GUEST_CAPS: u32 = wire::CAP_CLIPBOARD_BY_DEMAND | wire::CAP_CLIPBOARD_GRAB_SERIAL;

/// Each message's data bytes.
pub const CAPABILITIES_DATA: usize = 8;
pub const GRAB_DATA: usize = 8;
pub const REQUEST_DATA: usize = 4;
pub const RELEASE_DATA: usize = 0;
pub const fn clipboard_data(text: usize) -> usize {
    4 + text
}

/// The bytes one message of `data` bytes takes on the wire: its header and
/// data, and a chunk header before every [`MAX_CHUNK`] of them.
pub const fn message_bytes(data: usize) -> usize {
    let message = wire::MESSAGE_HEADER + data;
    message + wire::CHUNK_HEADER * message.div_ceil(MAX_CHUNK)
}

/// Messages written one after another into a buffer the caller sized.
///
/// # Panics
/// A message past the buffer's end: the caller sizes it to the largest
/// transmit it makes, so one past it is the caller's bug.
pub struct Writer<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl<'a> Writer<'a> {
    pub fn new(out: &'a mut [u8]) -> Self {
        Self { out, len: 0 }
    }

    /// The bytes written.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// `ANNOUNCE_CAPABILITIES`, `request` 0: the agent never asks for the
    /// host's.
    pub fn capabilities(&mut self) {
        self.message(wire::ANNOUNCE_CAPABILITIES, &[&0u32.to_le_bytes(), &GUEST_CAPS.to_le_bytes()]);
    }

    /// `GRAB [serial][UTF8_TEXT]`.
    pub fn grab(&mut self, serial: u32) {
        self.message(wire::GRAB, &[&serial.to_le_bytes(), &wire::UTF8_TEXT.to_le_bytes()]);
    }

    /// `REQUEST [UTF8_TEXT]`.
    pub fn request(&mut self) {
        self.message(wire::REQUEST, &[&wire::UTF8_TEXT.to_le_bytes()]);
    }

    /// `CLIPBOARD [UTF8_TEXT, text]`.
    pub fn clipboard(&mut self, text: &[u8]) {
        self.message(wire::CLIPBOARD, &[&wire::UTF8_TEXT.to_le_bytes(), text]);
    }

    /// `RELEASE []`.
    pub fn release(&mut self) {
        self.message(wire::RELEASE, &[]);
    }

    /// A message of `kind` whose data is `parts` in order, chunked.
    fn message(&mut self, kind: u32, parts: &[&[u8]]) {
        let data: usize = parts.iter().map(|p| p.len()).sum();
        let total = message_bytes(data);
        assert!(
            self.len + total <= self.out.len(),
            "vdagent: a {total}-byte message past a {}-byte transmit holding {}",
            self.out.len(),
            self.len
        );
        let size = u32::try_from(data).expect("a message's data fits its u32 size");
        let header = [
            &wire::PROTOCOL.to_le_bytes()[..],
            &kind.to_le_bytes()[..],
            &0u64.to_le_bytes()[..],
            &size.to_le_bytes()[..],
        ];
        let mut left = wire::MESSAGE_HEADER + data;
        let mut in_chunk = 0;
        for mut part in header.into_iter().chain(parts.iter().copied()) {
            while !part.is_empty() {
                if in_chunk == 0 {
                    let chunk = left.min(MAX_CHUNK);
                    self.put(&wire::CLIENT_PORT.to_le_bytes());
                    self.put(&(chunk as u32).to_le_bytes());
                    in_chunk = chunk;
                }
                let take = in_chunk.min(part.len());
                self.put(&part[..take]);
                part = &part[take..];
                in_chunk -= take;
                left -= take;
            }
        }
    }

    fn put(&mut self, bytes: &[u8]) {
        self.out[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }
}

#[cfg(test)]
mod tests {
    use std::vec;
    use std::vec::Vec;

    use super::*;

    fn written(write: impl FnOnce(&mut Writer)) -> Vec<u8> {
        let mut out = vec![0; crate::LARGEST_TRANSMIT];
        let mut writer = Writer::new(&mut out);
        write(&mut writer);
        let len = writer.len();
        out.truncate(len);
        out
    }

    /// `VDIChunkHeader {port 1, size}` then `VDAgentMessage {protocol 1, type,
    /// opaque 0, size}`, little-endian and packed.
    fn single(kind: u8, data: &[u8]) -> Vec<u8> {
        let size = data.len() as u8;
        let mut out = vec![1, 0, 0, 0, 20 + size, 0, 0, 0];
        out.extend([1, 0, 0, 0, kind, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, size, 0, 0, 0]);
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn capabilities_are_request_0_with_by_demand_and_grab_serial() {
        assert_eq!(written(|w| w.capabilities()), single(6, &[0, 0, 0, 0, 0x20, 0, 0x02, 0]));
        assert_eq!(message_bytes(CAPABILITIES_DATA), 36);
    }

    #[test]
    fn a_grab_carries_its_serial_and_utf8_text() {
        assert_eq!(written(|w| w.grab(0x0102_0304)), single(7, &[4, 3, 2, 1, 1, 0, 0, 0]));
        assert_eq!(message_bytes(GRAB_DATA), 36);
    }

    #[test]
    fn a_request_and_a_release_are_their_layouts() {
        assert_eq!(written(|w| w.request()), single(8, &[1, 0, 0, 0]));
        assert_eq!(written(|w| w.release()), single(9, &[]));
        assert_eq!(message_bytes(REQUEST_DATA), 32);
        assert_eq!(message_bytes(RELEASE_DATA), 28);
    }

    #[test]
    fn a_short_clipboard_is_one_chunk() {
        assert_eq!(written(|w| w.clipboard(b"hi")), single(4, &[1, 0, 0, 0, b'h', b'i']));
        assert_eq!(written(|w| w.clipboard(b"")), single(4, &[1, 0, 0, 0]));
    }

    /// A message past one chunk is cut every 2048 bytes of message, header
    /// included, each cut behind a chunk header of its own size.
    #[test]
    fn a_long_clipboard_is_chunked_at_2048() {
        let text: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        let out = written(|w| w.clipboard(&text));
        let mut message = single(4, &[1, 0, 0, 0])[8..].to_vec();
        message[16..20].copy_from_slice(&(4 + text.len() as u32).to_le_bytes());
        message.extend_from_slice(&text);
        let expected: Vec<u8> = message
            .chunks(2048)
            .flat_map(|c| [1u32.to_le_bytes(), (c.len() as u32).to_le_bytes()].concat().into_iter().chain(c.iter().copied()))
            .collect();
        assert_eq!(out, expected);
        assert_eq!(out.len(), message_bytes(clipboard_data(5000)));
    }

    /// The largest push is 2,105,412 bytes, the element QEMU 11.1.1 took and
    /// delivered whole, measured; with the capabilities before it, the largest
    /// transmit.
    #[test]
    fn the_largest_transmit_is_capabilities_and_a_whole_push() {
        assert_eq!(message_bytes(clipboard_data(crate::MAX_TEXT)), 2_105_376);
        assert_eq!(message_bytes(GRAB_DATA) + message_bytes(clipboard_data(crate::MAX_TEXT)), 2_105_412);
        assert_eq!(crate::LARGEST_TRANSMIT, 2_105_448);
        let text = vec![b'x'; crate::MAX_TEXT];
        let out = written(|w| {
            w.capabilities();
            w.grab(0);
            w.clipboard(&text);
        });
        assert_eq!(out.len(), crate::LARGEST_TRANSMIT);
    }

    #[test]
    #[should_panic(expected = "past a 64-byte transmit")]
    fn a_message_past_the_buffer_is_the_callers_bug() {
        let mut out = [0; 64];
        let mut writer = Writer::new(&mut out);
        writer.capabilities();
        writer.capabilities();
    }
}
