//! The host's vdagent stream, decoded as a stream: a `VDIChunkHeader {port,
//! size}` frames each run of a `VDAgentMessage {protocol, type, opaque, size}`
//! and its data, and a header can straddle two receive buffers.
//!
//! **Only a chunk past [`MAX_CHUNK`] ends the decode** ([`Unframed`]): no byte
//! after it can be placed. Everything else is refused by name and the next
//! message still decodes ([`Refusal`]): a chunk for another port is skipped, a
//! message of another protocol, past [`MAX_DATA`], of a type the agent does not
//! take or of a size its type does not have is refused at its header, and its
//! data counted off unread.
//!
//! **What is buffered grows with the bytes that arrive**, at most to twice
//! them and never past the size the header declares.
//!
//! Each type's size is its layout in `vd_agent.h` without the selection prefix,
//! which the agent never announces: QEMU 11.1.1 writes the prefix only to a
//! guest that announced `CLIPBOARD_SELECTION`, measured.

use alloc::vec::Vec;

use toyos_untrusted::Untrusted;

use crate::wire;

/// `VD_AGENT_MAX_DATA_SIZE`: the most one chunk carries.
pub const MAX_CHUNK: usize = 2048;

/// The most data a message may carry: a `CLIPBOARD`'s type and
/// [`crate::MAX_TEXT`] of text.
pub const MAX_DATA: usize = 4 + crate::MAX_TEXT;

/// The most types a `GRAB` may offer.
pub const MAX_GRAB_TYPES: usize = 16;

/// One message the host sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// `ANNOUNCE_CAPABILITIES`: whether the host asks for the agent's, and its
    /// first word of capability bits.
    Capabilities { request: bool, caps: u32 },
    /// `GRAB [serial][types]`.
    Grab { serial: u32, types: Vec<u32> },
    /// `REQUEST [type]`.
    Request { kind: u32 },
    /// `CLIPBOARD [type, data]`.
    Clipboard { kind: u32, data: Vec<u8> },
    /// `RELEASE []`.
    Release,
}

/// A part of the stream that is not a message, passed over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A chunk for a port other than `VDP_CLIENT_PORT`, skipped.
    OtherPort { port: u32 },
    /// A message whose `protocol` is not `VD_AGENT_PROTOCOL`.
    Protocol { protocol: u32 },
    /// A message declaring more than [`MAX_DATA`], counted off unread.
    TooLarge { kind: u32, size: u32 },
    /// A message whose size is not one its type has.
    Size { kind: u32, size: u32 },
    /// A message of a type the agent does not take.
    Kind { kind: u32 },
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OtherPort { port } => write!(f, "a chunk for port {port}, skipped"),
            Self::Protocol { protocol } => write!(f, "a message of protocol {protocol}, counted off"),
            Self::TooLarge { kind, size } => {
                write!(f, "a type {kind} message of {size} bytes, past {MAX_DATA}, counted off unread")
            }
            Self::Size { kind, size } => write!(f, "a type {kind} message of {size} bytes, no size its type has"),
            Self::Kind { kind } => write!(f, "a message of type {kind}, which this agent does not take"),
        }
    }
}

/// A chunk declared past [`MAX_CHUNK`]: no byte after it can be placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unframed {
    pub size: u32,
}

impl core::fmt::Display for Unframed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "a chunk of {} bytes, past {MAX_CHUNK}, leaves the stream unframed", self.size)
    }
}

/// A type the agent takes, its size judged against its layout.
#[derive(Clone, Copy)]
enum Kind {
    Capabilities,
    Grab,
    Request,
    Clipboard,
    Release,
}

/// Where the decode stands in the current message.
enum Body {
    /// Its header, `filled` bytes of it.
    Header { filled: usize },
    /// Its data, kept: `left` bytes still to come.
    Keep { kind: Kind, data: Vec<u8>, left: usize },
    /// Its data, counted off: `left` bytes still to come.
    Skip { left: usize },
}

/// The stream's decoder.
pub struct Decoder {
    chunk: [u8; wire::CHUNK_HEADER],
    /// Bytes of the chunk header read, while a header is being read.
    chunk_filled: usize,
    /// Bytes of the current chunk still to come, and whether it is the
    /// client port's.
    chunk_left: usize,
    ours: bool,
    header: [u8; wire::MESSAGE_HEADER],
    body: Body,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            chunk: [0; wire::CHUNK_HEADER],
            chunk_filled: 0,
            chunk_left: 0,
            ours: false,
            header: [0; wire::MESSAGE_HEADER],
            body: Body::Header { filled: 0 },
        }
    }
}

impl Decoder {
    /// Decode `bytes`, the next the host sent, handing each message or
    /// refusal to `each` in order.
    pub fn feed(&mut self, mut bytes: &[u8], mut each: impl FnMut(Result<Message, Refusal>)) -> Result<(), Unframed> {
        while !bytes.is_empty() {
            if self.chunk_left == 0 {
                let take = (wire::CHUNK_HEADER - self.chunk_filled).min(bytes.len());
                self.chunk[self.chunk_filled..self.chunk_filled + take].copy_from_slice(&bytes[..take]);
                self.chunk_filled += take;
                bytes = &bytes[take..];
                if self.chunk_filled < wire::CHUNK_HEADER {
                    break;
                }
                self.chunk_filled = 0;
                let port = word(&self.chunk, 0);
                let size = word(&self.chunk, 4);
                self.chunk_left = Untrusted::new(size)
                    .at_most(MAX_CHUNK as u64)
                    .map_err(|_| Unframed { size })? as usize;
                self.ours = port == wire::CLIENT_PORT;
                if !self.ours {
                    each(Err(Refusal::OtherPort { port }));
                }
                continue;
            }
            let take = self.chunk_left.min(bytes.len());
            if self.ours {
                self.message_bytes(&bytes[..take], &mut each);
            }
            self.chunk_left -= take;
            bytes = &bytes[take..];
        }
        Ok(())
    }

    /// The client port's chunk payload: the next of the current message.
    fn message_bytes(&mut self, mut bytes: &[u8], each: &mut impl FnMut(Result<Message, Refusal>)) {
        while !bytes.is_empty() {
            match &mut self.body {
                Body::Header { filled } => {
                    let take = (wire::MESSAGE_HEADER - *filled).min(bytes.len());
                    self.header[*filled..*filled + take].copy_from_slice(&bytes[..take]);
                    *filled += take;
                    bytes = &bytes[take..];
                    if *filled == wire::MESSAGE_HEADER {
                        self.body = self.begin(each);
                        if let Body::Keep { left: 0, .. } = self.body {
                            self.finish(each);
                        }
                    }
                }
                Body::Keep { data, left, .. } => {
                    let take = (*left).min(bytes.len());
                    // Doubling, as `Vec` grows, but never past what is declared.
                    if data.capacity() - data.len() < take {
                        let want = (data.len() * 2).max(data.len() + take).min(data.len() + *left);
                        data.reserve_exact(want - data.len());
                    }
                    data.extend_from_slice(&bytes[..take]);
                    *left -= take;
                    bytes = &bytes[take..];
                    if *left == 0 {
                        self.finish(each);
                    }
                }
                Body::Skip { left } => {
                    let take = (*left).min(bytes.len());
                    *left -= take;
                    bytes = &bytes[take..];
                    if *left == 0 {
                        self.body = Body::Header { filled: 0 };
                    }
                }
            }
        }
    }

    /// A whole message header is in: what its data is to be. Every refusal is
    /// made here, and the data of a refused message is counted off unread.
    fn begin(&self, each: &mut impl FnMut(Result<Message, Refusal>)) -> Body {
        let protocol = word(&self.header, 0);
        let kind = word(&self.header, 4);
        let size = word(&self.header, 16);
        match admit(protocol, kind, size) {
            Ok(kind) => Body::Keep { kind, data: Vec::new(), left: size as usize },
            Err(refusal) => {
                each(Err(refusal));
                Body::Skip { left: size as usize }
            }
        }
    }

    /// The current message is whole: decode it, and start the next.
    fn finish(&mut self, each: &mut impl FnMut(Result<Message, Refusal>)) {
        let Body::Keep { kind, data, .. } = core::mem::replace(&mut self.body, Body::Header { filled: 0 }) else {
            unreachable!("a message is finished only while its data is kept")
        };
        each(Ok(message(kind, data)));
    }
}

/// The type of a message of `protocol` declaring `size` bytes, where the agent
/// takes it and `size` is one its layout has.
fn admit(protocol: u32, kind: u32, size: u32) -> Result<Kind, Refusal> {
    if protocol != wire::PROTOCOL {
        return Err(Refusal::Protocol { protocol });
    }
    if Untrusted::new(size).at_most(MAX_DATA as u64).is_err() {
        return Err(Refusal::TooLarge { kind, size });
    }
    let len = size as usize;
    let (admitted, fits) = match kind {
        wire::ANNOUNCE_CAPABILITIES => (Kind::Capabilities, len >= 8),
        wire::GRAB => (Kind::Grab, len >= 4 && len.is_multiple_of(4) && (len - 4) / 4 <= MAX_GRAB_TYPES),
        wire::REQUEST => (Kind::Request, len == 4),
        wire::CLIPBOARD => (Kind::Clipboard, len >= 4),
        wire::RELEASE => (Kind::Release, len == 0),
        kind => return Err(Refusal::Kind { kind }),
    };
    match fits {
        true => Ok(admitted),
        false => Err(Refusal::Size { kind, size }),
    }
}

/// A whole message's data, whose size [`admit`] held to its type's layout.
fn message(kind: Kind, data: Vec<u8>) -> Message {
    match kind {
        Kind::Capabilities => Message::Capabilities { request: word(&data, 0) != 0, caps: word(&data, 4) },
        Kind::Grab => Message::Grab {
            serial: word(&data, 0),
            types: data[4..].as_chunks::<4>().0.iter().map(|t| u32::from_le_bytes(*t)).collect(),
        },
        Kind::Request => Message::Request { kind: word(&data, 0) },
        Kind::Clipboard => {
            let clip = word(&data, 0);
            let mut data = data;
            data.drain(..4);
            Message::Clipboard { kind: clip, data }
        }
        Kind::Release => Message::Release,
    }
}

/// The little-endian `u32` at `at`, which the caller has bounded.
fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[cfg(test)]
mod tests {
    use std::vec;
    use std::vec::Vec;

    use super::*;

    /// One chunk of `port` carrying `payload`.
    fn chunk(port: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&port.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// A message header and its data, declaring `size`.
    fn message_with(protocol: u32, kind: u32, size: u32, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&protocol.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    /// A message the host sends, chunked as QEMU 11.1.1 chunks it: 1024 bytes
    /// of message at most per chunk, measured.
    fn host(kind: u32, data: &[u8]) -> Vec<u8> {
        message_with(1, kind, data.len() as u32, data).chunks(1024).flat_map(|c| chunk(1, c)).collect()
    }

    fn words(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn decode(bytes: &[u8]) -> (Vec<Result<Message, Refusal>>, Result<(), Unframed>) {
        let mut out = Vec::new();
        let end = Decoder::default().feed(bytes, |m| out.push(m));
        (out, end)
    }

    fn grab(serial: u32) -> Message {
        Message::Grab { serial, types: vec![wire::UTF8_TEXT] }
    }

    /// Every message type, and a text that spans many chunks.
    fn valid_stream() -> (Vec<u8>, Vec<Result<Message, Refusal>>) {
        let text: Vec<u8> = (0..5000u32).map(|i| b'a' + (i % 26) as u8).collect();
        let mut clipboard = words(&[wire::UTF8_TEXT]);
        clipboard.extend_from_slice(&text);
        let mut bytes = host(wire::ANNOUNCE_CAPABILITIES, &words(&[1, 0x0002_0060]));
        bytes.extend(host(wire::GRAB, &words(&[3, wire::UTF8_TEXT])));
        bytes.extend(host(wire::REQUEST, &words(&[wire::UTF8_TEXT])));
        bytes.extend(host(wire::CLIPBOARD, &clipboard));
        bytes.extend(host(wire::RELEASE, &[]));
        let expected = vec![
            Ok(Message::Capabilities { request: true, caps: 0x0002_0060 }),
            Ok(grab(3)),
            Ok(Message::Request { kind: wire::UTF8_TEXT }),
            Ok(Message::Clipboard { kind: wire::UTF8_TEXT, data: text }),
            Ok(Message::Release),
        ];
        (bytes, expected)
    }

    /// A chunk or message header straddling two receive buffers decodes as
    /// though it had arrived whole, wherever the cut falls.
    #[test]
    fn a_valid_stream_decodes_the_same_at_every_split_point() {
        let (bytes, expected) = valid_stream();
        assert_eq!(decode(&bytes), (expected.clone(), Ok(())));
        for cut in 0..=bytes.len() {
            let mut out = Vec::new();
            let mut decoder = Decoder::default();
            decoder.feed(&bytes[..cut], |m| out.push(m)).unwrap();
            decoder.feed(&bytes[cut..], |m| out.push(m)).unwrap();
            assert_eq!(out, expected, "cut at {cut}");
        }
    }

    #[test]
    fn a_chunk_over_2048_leaves_the_stream_unframed() {
        let mut bytes = chunk(1, &[0; 2048]);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&2049u32.to_le_bytes());
        assert_eq!(decode(&bytes).1, Err(Unframed { size: 2049 }));
    }

    #[test]
    fn a_chunk_for_another_port_is_skipped() {
        let mut bytes = chunk(2, &host(wire::RELEASE, &[]));
        bytes.extend(host(wire::RELEASE, &[]));
        assert_eq!(decode(&bytes), (vec![Err(Refusal::OtherPort { port: 2 }), Ok(Message::Release)], Ok(())));
    }

    #[test]
    fn another_protocol_is_refused_and_counted_off() {
        let mut bytes = chunk(1, &message_with(2, wire::REQUEST, 4, &words(&[1])));
        bytes.extend(host(wire::RELEASE, &[]));
        assert_eq!(decode(&bytes), (vec![Err(Refusal::Protocol { protocol: 2 }), Ok(Message::Release)], Ok(())));
    }

    /// A message declaring one byte past the bound is refused at its header,
    /// its data counted off as it arrives, and the message after it decodes.
    #[test]
    fn a_declared_size_past_the_bound_is_counted_off_and_the_next_message_decodes() {
        let size = MAX_DATA as u32 + 1;
        let message: Vec<u8> = message_with(1, wire::CLIPBOARD, size, &vec![7; size as usize]);
        let mut bytes: Vec<u8> = message.chunks(MAX_CHUNK).flat_map(|c| chunk(1, c)).collect();
        bytes.extend(host(wire::REQUEST, &words(&[wire::UTF8_TEXT])));
        let mut out = Vec::new();
        let mut decoder = Decoder::default();
        decoder.feed(&bytes, |m| out.push(m)).unwrap();
        assert_eq!(
            out,
            vec![
                Err(Refusal::TooLarge { kind: wire::CLIPBOARD, size }),
                Ok(Message::Request { kind: wire::UTF8_TEXT })
            ]
        );
        let Body::Header { filled: 0 } = decoder.body else { panic!("the decoder kept a body") };
    }

    /// The largest message the bound admits decodes whole.
    #[test]
    fn a_message_at_the_bound_decodes() {
        let mut data = words(&[wire::UTF8_TEXT]);
        data.extend(vec![b'x'; crate::MAX_TEXT]);
        let (out, end) = decode(&host(wire::CLIPBOARD, &data));
        assert_eq!(end, Ok(()));
        assert_eq!(out, vec![Ok(Message::Clipboard { kind: wire::UTF8_TEXT, data: vec![b'x'; crate::MAX_TEXT] })]);
    }

    #[test]
    fn each_type_is_held_to_its_size() {
        for (kind, data) in [
            (wire::REQUEST, words(&[1, 1])),
            (wire::REQUEST, vec![1, 0]),
            (wire::RELEASE, words(&[0])),
            (wire::ANNOUNCE_CAPABILITIES, words(&[1])),
            (wire::GRAB, vec![]),
            (wire::GRAB, vec![0, 0, 0, 0, 1]),
            (wire::GRAB, words(&[0; 18])),
            (wire::CLIPBOARD, vec![1, 0, 0]),
        ] {
            let size = data.len() as u32;
            assert_eq!(decode(&host(kind, &data)).0, vec![Err(Refusal::Size { kind, size })], "type {kind}");
        }
        assert_eq!(
            decode(&host(wire::GRAB, &words(&[0; 17]))).0,
            vec![Ok(Message::Grab { serial: 0, types: vec![0; 16] })]
        );
        assert_eq!(decode(&host(wire::GRAB, &words(&[5]))).0, vec![Ok(Message::Grab { serial: 5, types: vec![] })]);
    }

    /// Only the header has arrived: a message the agent would refuse is named
    /// before any of its data is buffered.
    #[test]
    fn a_refusal_is_named_at_its_header_before_its_data_arrives() {
        for (protocol, kind, size, refusal) in [
            (2, wire::REQUEST, 4, Refusal::Protocol { protocol: 2 }),
            (1, wire::REQUEST, 1000, Refusal::Size { kind: wire::REQUEST, size: 1000 }),
            (1, wire::RELEASE, 1000, Refusal::Size { kind: wire::RELEASE, size: 1000 }),
            (1, wire::GRAB, 1000, Refusal::Size { kind: wire::GRAB, size: 1000 }),
            (1, 1, 1000, Refusal::Kind { kind: 1 }),
        ] {
            let header = message_with(protocol, kind, size, &[]);
            // A chunk declaring the whole message, of which only the header came.
            let mut bytes = words(&[1, header.len() as u32 + size]);
            bytes.extend(header);
            assert_eq!(decode(&bytes), (vec![Err(refusal)], Ok(())), "type {kind}");
        }
    }

    /// A message fed as QEMU 11.1.1 chunks it keeps no more than it declares:
    /// one whose second chunk outgrows twice the first, and the largest the
    /// bound admits.
    #[test]
    fn a_kept_message_buffers_no_more_than_it_declares() {
        for text in [2048, crate::MAX_TEXT] {
            let mut data = words(&[wire::UTF8_TEXT]);
            data.extend(vec![b'x'; text]);
            let declared = data.len();
            let (out, _) = decode(&host(wire::CLIPBOARD, &data));
            let [Ok(Message::Clipboard { data, .. })] = out.as_slice() else { panic!("{} messages", out.len()) };
            assert!(data.capacity() <= declared, "{} bytes held for {declared} declared", data.capacity());
        }
    }

    #[test]
    fn an_unknown_type_is_refused() {
        let mut bytes = host(1, &[0; 12]);
        bytes.extend(host(wire::RELEASE, &[]));
        assert_eq!(decode(&bytes), (vec![Err(Refusal::Kind { kind: 1 }), Ok(Message::Release)], Ok(())));
    }
}
