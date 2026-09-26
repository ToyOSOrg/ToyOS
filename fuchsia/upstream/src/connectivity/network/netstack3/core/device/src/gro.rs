// Copyright 2026 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Constructs that support Generic Receive Offload (GRO) at the device layer.

use alloc::vec::Vec;
use core::num::NonZeroU16;

use assert_matches::assert_matches;
use derivative::Derivative;
use net_types::ethernet::Mac;
use net_types::for_any_ip_version;
use net_types::ip::{IpAddress, IpVersion, Ipv4, Ipv4Addr, Ipv6, Ipv6Addr};
use packet::ParsablePacket;
use packet_formats::ethernet::{EtherType, EthernetFrame, EthernetFrameLengthCheck};
use packet_formats::ip::{DscpAndEcn, IpExt, IpProto, Ipv4Proto, Ipv6Proto};
use packet_formats::ipv4::{Ipv4Header, Ipv4Packet, Ipv4PacketRaw};
use packet_formats::ipv6::{IPV6_FIXED_HDR_LEN, Ipv6Header, Ipv6Packet, Ipv6PacketRaw};
use packet_formats::tcp::{MAX_OPTIONS_LEN, TcpParseArgs, TcpSegment, TcpSegmentRaw};

use netstack3_base::{ChecksumRxOffloading, GsoInfo, Ipv4IdMode, NetworkParsingContext};

/// The maximum length of a coalesced frame.
///
/// Chosen so that the lengths derived from a coalesced frame (IPv4 total
/// length, IPv6 payload length, and the transport packet length that's part of
/// the IP pseudo-header used in transport-layer checksum calculation) always
/// fit in 16 bits (the size allotted for each of these values, with the
/// exception of the transport length in the IPv6 pseudo-header which is 32
/// bits). This is a conservative bound: the frame length also includes the
/// link-layer and IP headers, so each of those lengths is strictly smaller than
/// the frame length.
const MAX_GRO_FRAME_LEN: u16 = u16::MAX;

/// Returns the number of bytes that can still be coalesced into a frame whose
/// current length is `frame_len`.
fn gro_headroom(frame_len: usize) -> usize {
    usize::from(MAX_GRO_FRAME_LEN).saturating_sub(frame_len)
}

/// A slice view of a buffer, which is either a contiguous slice or linearized
/// into scratch storage.
#[derive(Debug)]
pub enum BufferSlice<'a, 'b> {
    /// A slice view directly into a contiguous buffer.
    Contiguous(&'a mut [u8]),
    /// A slice view into scratch storage after linearizing a non-contiguous
    /// buffer.
    Linearized(&'b mut [u8]),
}

impl BufferSlice<'_, '_> {
    /// Returns an immutable slice view of the buffer.
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Contiguous(s) => s,
            Self::Linearized(s) => s,
        }
    }

    /// Returns a mutable slice view of the buffer.
    pub fn as_slice_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Contiguous(s) => s,
            Self::Linearized(s) => s,
        }
    }
}

/// A buffer that may be backed by a contiguous memory slice.
pub trait MaybeContiguousBuffer {
    /// Obtains a slice view into the buffer, linearizing into `storage` if
    /// necessary, or returning `None` if linearization was required and
    /// `storage` was `None`.
    fn linearized<'a, 'b>(
        &'a mut self,
        storage: Option<&'b mut Vec<u8>>,
    ) -> Option<BufferSlice<'a, 'b>>;

    /// Obtains a mutable slice into the buffer without linearizing. Panics if
    /// called on a buffer that's not contiguous.
    fn unwrap_contiguous<'a>(&'a mut self) -> &'a mut [u8] {
        assert_matches!(
            self.linearized(None).expect("must be `Some` if contiguous"),
            BufferSlice::Contiguous(slice) => slice
        )
    }
}

/// Frame type for GRO packet parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroFrameType {
    /// Ethernet frame.
    Ethernet,
    /// Pure IP frame (IPv4 or IPv6).
    PureIp(IpVersion),
}

/// The destination for a buffer involved in GRO (e.g., a device ID). Buffers
/// with different destinations are not coalesced.
pub trait GroBufferDestination: Eq {
    /// Returns the frame type for this destination.
    fn frame_type(&self) -> GroFrameType;
}

/// Ethernet flow identifier for GRO matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EthernetFlowId {
    src_mac: Mac,
    dst_mac: Mac,
    tag: Option<u32>,
}

/// Link layer flow identifier for GRO matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkLayerFlowId {
    Ethernet(EthernetFlowId),
    PureIp,
}

/// Link-layer framing information used to derive GRO eligibility and flow ID.
enum LinkLayerFrame {
    Ethernet { flow_id: EthernetFlowId, ethertype: Option<EtherType> },
    PureIp(IpVersion),
}

impl LinkLayerFrame {
    fn parse<T: GroBufferDestination>(slice: &mut &[u8], target: &T) -> Option<LinkLayerFrame> {
        match target.frame_type() {
            GroFrameType::Ethernet => {
                let frame = EthernetFrame::parse(slice, EthernetFrameLengthCheck::NoCheck).ok()?;
                let flow_id = EthernetFlowId {
                    src_mac: frame.src_mac(),
                    dst_mac: frame.dst_mac(),
                    tag: frame.tag(),
                };
                Some(LinkLayerFrame::Ethernet { flow_id, ethertype: frame.ethertype() })
            }
            GroFrameType::PureIp(ip_version) => Some(LinkLayerFrame::PureIp(ip_version)),
        }
    }

    fn is_eligible_for_gro(&self) -> bool {
        match self {
            Self::Ethernet { .. } => true,
            Self::PureIp(_) => true,
        }
    }

    fn ethertype(&self) -> Option<EtherType> {
        match self {
            Self::Ethernet { ethertype, .. } => *ethertype,
            Self::PureIp(ip_version) => Some(EtherType::from_ip_version(*ip_version)),
        }
    }

    fn flow_id(&self) -> LinkLayerFlowId {
        match self {
            Self::Ethernet { flow_id, .. } => LinkLayerFlowId::Ethernet(*flow_id),
            Self::PureIp(_) => LinkLayerFlowId::PureIp,
        }
    }
}

/// IPv4 flow identifier for GRO matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ipv4FlowId {
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
}

/// IPv6 flow identifier for GRO matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ipv6FlowId {
    src_ip: Ipv6Addr,
    dst_ip: Ipv6Addr,
    flowlabel: u32,
}

/// IP layer flow identifier for GRO matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpFlowId {
    Ipv4(Ipv4FlowId),
    Ipv6(Ipv6FlowId),
}

impl IpFlowId {
    /// Returns the partial sum of `segment`'s payload, computed from
    /// `segment`'s checksum and this flow's pseudo header addresses.
    fn recover_payload_partial_sum(&self, segment: &TcpSegment<&'_ [u8]>) -> [u8; 2] {
        fn recover<A: IpAddress>(src_ip: A, dst_ip: A, segment: &TcpSegment<&'_ [u8]>) -> [u8; 2] {
            segment
                .recover_payload_partial_sum::<A::Version>(src_ip, dst_ip)
                .expect("transport len fits in IP total length")
        }

        match self {
            IpFlowId::Ipv4(Ipv4FlowId { src_ip, dst_ip }) => recover(*src_ip, *dst_ip, segment),
            IpFlowId::Ipv6(Ipv6FlowId { src_ip, dst_ip, flowlabel: _ }) => {
                recover(*src_ip, *dst_ip, segment)
            }
        }
    }
}

enum IpPacket<'a> {
    V4(Ipv4Packet<&'a [u8]>),
    V6(Ipv6Packet<&'a [u8]>),
}

impl<'a> From<Ipv4Packet<&'a [u8]>> for IpPacket<'a> {
    fn from(packet: Ipv4Packet<&'a [u8]>) -> IpPacket<'a> {
        IpPacket::V4(packet)
    }
}

impl<'a> From<Ipv6Packet<&'a [u8]>> for IpPacket<'a> {
    fn from(packet: Ipv6Packet<&'a [u8]>) -> IpPacket<'a> {
        IpPacket::V6(packet)
    }
}

impl IpPacket<'_> {
    fn total_len(&self) -> usize {
        match self {
            IpPacket::V4(packet) => packet.header_len() + packet.body().len(),
            IpPacket::V6(packet) => packet.header_len() + packet.body().len(),
        }
    }
}

trait GroIpPacket<I: IpExt> {
    fn is_eligible_for_gro(&self) -> bool;
    fn ip_proto(&self) -> Option<IpProto>;
    fn flow_id(&self) -> IpFlowId;
}

impl GroIpPacket<Ipv4> for Ipv4Packet<&[u8]> {
    fn is_eligible_for_gro(&self) -> bool {
        // Must not be fragmented: the transport headers are only
        // available for flow matching in the first fragment.
        self.fragment_offset() == packet_formats::ip::FragmentOffset::ZERO
            && !self.mf_flag()
            // Must not have options: this lets us avoid the need to
            // copy them out of the buffer or arbitrarily index into the
            // coalescing buffer to find them.
            && self.header_len() == packet_formats::ipv4::HDR_PREFIX_LEN
    }

    fn ip_proto(&self) -> Option<IpProto> {
        match self.proto() {
            Ipv4Proto::Proto(proto) => Some(proto),
            Ipv4Proto::Icmp | Ipv4Proto::Igmp | Ipv4Proto::Other(_) => None,
        }
    }

    fn flow_id(&self) -> IpFlowId {
        IpFlowId::Ipv4(Ipv4FlowId { src_ip: self.src_ip(), dst_ip: self.dst_ip() })
    }
}

impl GroIpPacket<Ipv6> for Ipv6Packet<&[u8]> {
    fn is_eligible_for_gro(&self) -> bool {
        // Must not have extension headers. Note that this differs from
        // Linux, which allows extension headers as long as they're
        // equal. We omit these for the same reason as the IPv4 case
        // above: to avoid needing to copy them or arbitrarily index
        // into the coalescing buffer.
        self.iter_extension_hdrs().next().is_none()
    }

    fn ip_proto(&self) -> Option<IpProto> {
        match self.proto() {
            Ipv6Proto::Proto(proto) => Some(proto),
            Ipv6Proto::Icmpv6 | Ipv6Proto::NoNextHeader | Ipv6Proto::Other(_) => None,
        }
    }

    fn flow_id(&self) -> IpFlowId {
        IpFlowId::Ipv6(Ipv6FlowId {
            src_ip: self.src_ip(),
            dst_ip: self.dst_ip(),
            flowlabel: self.flowlabel(),
        })
    }
}

/// TCP flow identifier for GRO matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TcpFlowId {
    src_port: NonZeroU16,
    dst_port: NonZeroU16,
}

/// Transport layer flow identifier for GRO matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportFlowId {
    Tcp(TcpFlowId),
}

fn flag_requires_flush(segment: &TcpSegment<&'_ [u8]>) -> bool {
    segment.syn() || segment.fin() || segment.rst() || segment.urg() || segment.psh()
}

enum TransportPacket<'a> {
    Tcp(TcpSegment<&'a [u8]>),
}

impl<'a> TransportPacket<'a> {
    fn parse<A: IpAddress>(
        transport_view: &mut &'a [u8],
        proto: IpProto,
        src_ip: A,
        dst_ip: A,
        context: &mut NetworkParsingContext,
    ) -> Option<TransportPacket<'a>> {
        match proto {
            IpProto::Tcp => TcpSegment::parse(
                transport_view,
                TcpParseArgs::with_context(src_ip, dst_ip, context),
            )
            .ok()
            .map(TransportPacket::Tcp),
            // TODO(https://fxbug.dev/555942793): Implement GRO for UDP.
            IpProto::Udp | IpProto::Reserved => None,
        }
    }

    fn is_eligible_for_gro(&self) -> bool {
        match self {
            Self::Tcp(_) => true,
        }
    }

    fn flow_id(&self) -> TransportFlowId {
        match self {
            Self::Tcp(tcp) => TransportFlowId::Tcp(TcpFlowId {
                src_port: tcp.src_port(),
                dst_port: tcp.dst_port(),
            }),
        }
    }

    /// Checks if this packet must be flushed to the stack given that it was not
    /// merged into a flow, either because it didn't match one or because it
    /// failed the merge checks.
    ///
    /// If it need not be flushed, it's eligible to become a new flow.
    fn flush_if_not_merged(&self) -> bool {
        match self {
            Self::Tcp(tcp) => {
                let is_payload_segment = !tcp.body().is_empty();
                !is_payload_segment || flag_requires_flush(tcp)
            }
        }
    }
}

/// Flow identifier for GRO matching.
///
/// Note: if two packets have matching flow identifiers, it's not necessarily
/// true that they'll be coalesced, but it is true that their fates (coalesced,
/// flushed, or both) are shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GroFlowId {
    link_layer: LinkLayerFlowId,
    ip: IpFlowId,
    transport: TransportFlowId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeaderOffsets {
    pub ip_offset: usize,
    pub transport_offset: usize,
}

/// Constructs the checksum offloading indication to include in the GRO output
/// given the input offloading indication and the parsing context used to parse
/// the GRO packet.
fn build_csum_offload_output(
    input: ChecksumRxOffloading,
    context_post_parse: &NetworkParsingContext,
) -> ChecksumRxOffloading {
    let verified = context_post_parse.verified_checksum_count();
    match input {
        ChecksumRxOffloading::FullyOffloaded => ChecksumRxOffloading::FullyOffloaded,
        ChecksumRxOffloading::Offloaded(Some(n)) => {
            ChecksumRxOffloading::Offloaded(Some(n.saturating_add(verified)))
        }
        ChecksumRxOffloading::Offloaded(None) => {
            ChecksumRxOffloading::Offloaded(NonZeroU16::new(verified))
        }
    }
}

/// Parsed packet containing all metadata needed for GRO matching and
/// accumulation.
struct GroPacket<'a> {
    pub flow_id: GroFlowId,
    pub offsets: HeaderOffsets,
    pub ip: IpPacket<'a>,
    pub transport: TransportPacket<'a>,
}

impl<'a> GroPacket<'a> {
    fn parse<T: GroBufferDestination>(
        slice: &'a [u8],
        target: &T,
        context: &mut NetworkParsingContext,
    ) -> Option<GroPacket<'a>> {
        let total_len = slice.len();
        let mut view = slice;

        let ll_frame =
            LinkLayerFrame::parse(&mut view, target).filter(|f| f.is_eligible_for_gro())?;
        let ethertype = ll_frame.ethertype()?;

        let ip_offset = total_len - view.len();
        let ip_version = ethertype.to_ip_version()?;
        let (ip, ip_flow_id, transport_offset, transport) = for_any_ip_version!(ip_version, I, {
            let ip = <I as IpExt>::Packet::parse(&mut view, ())
                .ok()
                .filter(|p| p.is_eligible_for_gro())?;
            let flow_id = ip.flow_id();
            let proto = ip.ip_proto()?;
            let src = ip.src_ip();
            let dst = ip.dst_ip();

            // NB: the IP parser trims any bytes past the end of the IP payload
            // (e.g. link layer padding) off of `view`, so `total_len -
            // view.len()` would overshoot the start of the transport header by
            // the number of trailing bytes. Derive the offset from the IP
            // header length instead.
            let transport_offset = ip_offset + ip.header_len();
            let transport = TransportPacket::parse(&mut view, proto, src, dst, context)
                .filter(|p| p.is_eligible_for_gro())?;

            (ip.into(), flow_id, transport_offset, transport)
        });

        let offsets = HeaderOffsets { ip_offset, transport_offset };
        let flow_id = GroFlowId {
            link_layer: ll_frame.flow_id(),
            ip: ip_flow_id,
            transport: transport.flow_id(),
        };

        Some(GroPacket { flow_id, offsets, ip, transport })
    }

    /// The offset of the end of the IP payload in the frame this packet was
    /// parsed from, as indicated by the length field in its IP header.
    ///
    /// Anything past this point is a trailing byte that is not part of the
    /// packet, typically link layer padding on a frame smaller than the link
    /// layer minimum.
    ///
    /// Note that this can never be past the end of the frame: the IP parsers
    /// reject a frame whose body is shorter than its header claims, so such a
    /// frame never makes it this far.
    fn payload_end(&self) -> usize {
        self.offsets.ip_offset + self.ip.total_len()
    }

    /// Checks if this packet must be flushed to the stack given that it was
    /// not merged into a flow.
    fn flush_if_not_merged(&self, frame_len: usize) -> bool {
        // A frame that is already at the maximum coalesced length can never
        // accept another segment, so don't let it establish a flow. This also
        // guarantees that every flow's accumulated transport packet length can
        // be represented in the IP pseudo-header.
        gro_headroom(frame_len) == 0 || self.transport.flush_if_not_merged()
    }
}

enum CoalesceResult {
    Flush,
    Continue,
}

/// Encapsulates raw TCP options bytes up to maximum supported options length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TcpOptions {
    bytes: [u8; MAX_OPTIONS_LEN],
    len: usize,
}

impl TcpOptions {
    fn from_slice(slice: &[u8]) -> Self {
        let mut bytes = [0u8; MAX_OPTIONS_LEN];
        // Note: we're depending on TCP segment parsing (specifically
        // `TcpSegmentRaw::parse`) to uphold the invariant that this length is
        // <= `MAX_OPTIONS_LEN`.
        let len = slice.len();
        bytes[..len].copy_from_slice(slice);
        Self { bytes, len }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

/// The flags that coalesced segments are allowed to disagree on.
const PSH_OR_FIN: u8 = packet_formats::tcp::flags::PSH | packet_formats::tcp::flags::FIN;

/// TCP flow matching criteria and accumulation state.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TcpFlow {
    flags: u8,
    reserved_bits: u8,
    ack: Option<u32>,
    win: u16,
    options: TcpOptions,
    /// The sequence number that the next coalesced segment must start at.
    ///
    /// This only counts payload bytes; it ignores the virtual sequence number
    /// length consumed by a `FIN`. That's fine because anything received after
    /// a `FIN` is discarded by the state machine, so a segment that would only
    /// match the `FIN`-inclusive value can't be usefully coalesced anyways.
    next_seq: u32,
    checksum: [u8; 2],
    /// The total length of the first segment in the flow (header, options, and
    /// payload), i.e. the pseudo header length that is covered by the
    /// accumulated `checksum`.
    ///
    /// The difference between this and the final coalesced length is applied to
    /// the checksum once in `finalize`.
    orig_tcp_len: usize,
    /// Whether the accumulated length of the coalesced TCP segment (header,
    /// options, and payload so far) is odd.
    ///
    /// Determines whether the next coalesced payload starts on an odd byte
    /// offset; see [`TcpFlow::update_checksum`].
    tcp_len_is_odd: bool,
    /// The maximum size of the individual segment payloads that comprise the
    /// coalesced segment. These are all required to have the same length, with
    /// the possible exception of the last which is permitted to be shorter.
    ///
    /// This is tracked so that the segment can be faithfully resegmented in the
    /// event that it needs to be forwarded.
    gso_size: NonZeroU16,
}

impl TcpFlow {
    fn new(segment: &TcpSegment<&'_ [u8]>) -> Self {
        let payload_len = u16::try_from(segment.body().len()).expect("payload len fits in u16");
        let next_seq = segment.seq_num().wrapping_add(u32::from(payload_len));
        let options = TcpOptions::from_slice(segment.options().bytes());
        // `TransportFlow::flush_if_not_merged` ensures that only payload
        // segments can start GRO flows, so it's safe to assume here that
        // payload length is nonzero.
        let gso_size = NonZeroU16::new(payload_len).expect("payload len is non-zero");
        let tcp_len = segment.total_segment_len();
        Self {
            flags: segment.flags(),
            reserved_bits: segment.reserved_bits(),
            ack: segment.ack_num(),
            win: segment.window_size(),
            options,
            next_seq,
            checksum: segment.checksum(),
            orig_tcp_len: tcp_len,
            tcp_len_is_odd: tcp_len % 2 == 1,
            gso_size,
        }
    }

    /// Determines whether a TCP segment already matched to this flow is
    /// permitted to coalesce with it.
    fn can_coalesce(&self, current_frame_len: usize, segment: &TcpSegment<&'_ [u8]>) -> bool {
        let Self {
            flags,
            reserved_bits,
            ack,
            win,
            options,
            next_seq,
            checksum: _,
            orig_tcp_len: _,
            tcp_len_is_odd: _,
            gso_size,
        } = self;
        let payload_len = segment.body().len();
        // All coalesced segments must have size `gso_size` save for the last,
        // which may be smaller but cannot be larger.
        if payload_len == 0 || payload_len > usize::from(gso_size.get()) {
            return false;
        }
        payload_len <= gro_headroom(current_frame_len)
            // Coalesced segments' flags may only disagree on `PSH` or `FIN`.
            // The coalesced segment will receive the union of the flags.
            && (*flags & !PSH_OR_FIN) == (segment.flags() & !PSH_OR_FIN)
            // The reserved bits must match exactly; they may carry semantics
            // that we don't know about.
            && *reserved_bits == segment.reserved_bits()
            && *ack == segment.ack_num()
            && *win == segment.window_size()
            && options.as_bytes() == segment.options().bytes()
            // Sequence numbers must be contiguous.
            && *next_seq == segment.seq_num()
    }

    /// Updates the accumulated checksum to cover `segment`'s payload.
    ///
    /// The pseudo header length is not updated here; it is applied once in
    /// `finalize`.
    fn update_checksum(&mut self, ip_flow: &IpFlowId, segment: &TcpSegment<&'_ [u8]>) {
        let mut partial_sum = ip_flow.recover_payload_partial_sum(segment);
        // The TCP checksum is computed by summing the pseudo-header and segment
        // two bytes at a time. If the accumulated segment length is odd, then
        // the newly added payload would start in the middle of a two-byte pair.
        // Because the Internet Checksum is byte-order independent, we can
        // correct for this by swapping the bytes of the new payload's partial
        // sum before adding it to our checksum.
        if self.tcp_len_is_odd {
            partial_sum = [partial_sum[1], partial_sum[0]];
        }
        self.checksum = internet_checksum::add(self.checksum, &partial_sum);
    }

    /// Coalesces a TCP segment into this flow.
    ///
    /// Note that this operation does not modify the header of the coalesced TCP
    /// segment. Instead it performs internal bookkeeping and performs the
    /// modifications once in `finalize`.
    fn coalesce(
        &mut self,
        ip_flow: &IpFlowId,
        segment: &TcpSegment<&'_ [u8]>,
        coalesce_into: &mut Vec<u8>,
    ) -> CoalesceResult {
        coalesce_into.extend_from_slice(segment.body());
        let added_payload_len =
            u16::try_from(segment.body().len()).expect("payload len fits in u16");
        self.update_checksum(ip_flow, segment);

        self.tcp_len_is_odd ^= added_payload_len % 2 == 1;
        self.next_seq = segment.seq_num().wrapping_add(u32::from(added_payload_len));
        self.flags |= segment.flags() & PSH_OR_FIN;

        // All GRO'd segments must be the same length, save for the last one,
        // which may be shorter. This allows us to pass a single value up the
        // stack to indicate the original segment size and ensure that we can
        // resegment along the original segment boundaries if we end up
        // forwarding the frame.
        let is_smaller_than_gso = added_payload_len < self.gso_size.get();
        if is_smaller_than_gso || flag_requires_flush(segment) {
            CoalesceResult::Flush
        } else {
            CoalesceResult::Continue
        }
    }

    /// Finalizes the coalesced TCP segment by updating the header with the
    /// correct checksum and the merged flags (which may only set `FIN` and/or
    /// `PSH`).
    ///
    /// Returns the GSO segment size (the payload length of each coalesced
    /// segment).
    fn finalize(&self, mut transport_view: &mut [u8]) -> NonZeroU16 {
        // By the time the flow is finalized, the view holds exactly the
        // coalesced segment: any trailing bytes on the seed frame were trimmed
        // before the first payload was appended.
        let tcp_len = transport_view.len();
        let mut tcp =
            TcpSegmentRaw::parse_mut(&mut transport_view, ()).expect("valid TCP segment header");

        // NB: the IPv6 pseudo header carries a 32-bit upper layer length where
        // the IPv4 pseudo header carries a 16-bit TCP length.
        // `MAX_GRO_FRAME_LENGTH` ensures that our TCP segment length can be
        // represented by the latter, but we don't bother casting from `usize`
        // because the upper bits must all be zero anyways and thus have no
        // effect on the checksum.
        let checksum = internet_checksum::update(
            self.checksum,
            &self.orig_tcp_len.to_be_bytes(),
            &tcp_len.to_be_bytes(),
        );

        tcp.set_checksum(checksum);
        tcp.set_flags(self.flags);
        self.gso_size
    }
}

/// Transport-specific flow state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TransportFlow {
    Tcp(TcpFlow),
}

impl TransportFlow {
    fn new(packet: &TransportPacket<'_>) -> Self {
        match packet {
            TransportPacket::Tcp(tcp) => Self::Tcp(TcpFlow::new(tcp)),
        }
    }

    fn can_coalesce(&self, current_frame_len: usize, packet: &TransportPacket<'_>) -> bool {
        match (self, packet) {
            (Self::Tcp(flow), TransportPacket::Tcp(tcp)) => {
                flow.can_coalesce(current_frame_len, tcp)
            }
        }
    }

    fn coalesce(
        &mut self,
        ip_flow: &IpFlowId,
        packet: &TransportPacket<'_>,
        coalesce_into: &mut Vec<u8>,
    ) -> CoalesceResult {
        match (self, packet) {
            (Self::Tcp(flow), TransportPacket::Tcp(tcp)) => {
                flow.coalesce(ip_flow, tcp, coalesce_into)
            }
        }
    }

    fn finalize(&self, transport_view: &mut [u8]) -> NonZeroU16 {
        match self {
            Self::Tcp(tcp) => tcp.finalize(transport_view),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ipv4IdState {
    /// Only one packet seen so far with this ID.
    Initial(u16),
    /// IDs are consistent across packets.
    Consistent(u16),
    /// IDs are increasing by 1 across packets; stores the last seen ID.
    Increasing(u16),
}

impl Ipv4IdState {
    fn can_coalesce(&self, next_id: u16) -> bool {
        match self {
            Self::Initial(id) => next_id == *id || next_id == id.wrapping_add(1),
            Self::Consistent(id) => next_id == *id,
            Self::Increasing(prev_id) => next_id == prev_id.wrapping_add(1),
        }
    }

    fn coalesce(&mut self, next_id: u16) {
        *self = match self {
            Self::Initial(id) => {
                if next_id == *id {
                    Self::Consistent(next_id)
                } else if next_id == id.wrapping_add(1) {
                    Self::Increasing(next_id)
                } else {
                    unreachable!("cannot coalesce non-matching ID");
                }
            }
            Self::Consistent(id) => {
                debug_assert_eq!(next_id, *id);
                Self::Consistent(next_id)
            }
            Self::Increasing(_) => Self::Increasing(next_id),
        };
    }
}

/// IPv4 flow post-match fields and header finalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ipv4Flow {
    ttl: u8,
    dscp_and_ecn: DscpAndEcn,
    df_flag: bool,
    id_state: Ipv4IdState,
}

impl Ipv4Flow {
    fn new(packet: &impl Ipv4Header) -> Ipv4Flow {
        Self {
            ttl: packet.ttl(),
            dscp_and_ecn: packet.dscp_and_ecn(),
            df_flag: packet.df_flag(),
            id_state: Ipv4IdState::Initial(packet.id()),
        }
    }

    /// Determines whether an IPv4 packet already matched to this flow is
    /// permitted to coalesce with it.
    fn can_coalesce(&self, packet: &impl Ipv4Header) -> bool {
        let Self { ttl, dscp_and_ecn, df_flag, id_state } = self;
        *ttl == packet.ttl()
            && *dscp_and_ecn == packet.dscp_and_ecn()
            && *df_flag == packet.df_flag()
            && id_state.can_coalesce(packet.id())
    }

    fn coalesce(&mut self, packet: &impl Ipv4Header) {
        self.id_state.coalesce(packet.id());
    }

    /// Finalizes the coalesced IPv4 packet by updating the header with the new
    /// length and checksum.
    fn finalize(&self, mut ip_view: &mut [u8]) {
        let total_len = ip_view.len();
        let mut ip = Ipv4PacketRaw::parse_mut(&mut ip_view, ()).expect("valid IPv4 header");
        let new_len_u16 = u16::try_from(total_len).expect("IP total length fits in u16");
        ip.set_total_len_and_update_checksum(new_len_u16);
    }

    fn ipv4_id_mode(&self) -> Ipv4IdMode {
        match self.id_state {
            Ipv4IdState::Consistent(_) => Ipv4IdMode::Fixed,
            Ipv4IdState::Increasing(_) => Ipv4IdMode::Incrementing,
            Ipv4IdState::Initial(_) => {
                unreachable!("coalesced flow with num_coalesced > 1 cannot be Initial")
            }
        }
    }
}

/// IPv6 flow post-match fields and header finalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ipv6Flow {
    hop_limit: u8,
    dscp_and_ecn: DscpAndEcn,
}

impl Ipv6Flow {
    fn new(packet: &impl Ipv6Header) -> Ipv6Flow {
        Self { hop_limit: packet.hop_limit(), dscp_and_ecn: packet.dscp_and_ecn() }
    }

    /// Determines whether an IPv6 packet already matched to this flow is
    /// permitted to coalesce with it.
    fn can_coalesce(&self, packet: &impl Ipv6Header) -> bool {
        let Self { hop_limit, dscp_and_ecn } = self;
        *hop_limit == packet.hop_limit() && *dscp_and_ecn == packet.dscp_and_ecn()
    }

    fn coalesce(&mut self, _packet: &impl Ipv6Header) {
        // This is a no-op: unlike IPv4, which must track the identification
        // field across coalesced packets, no IPv6 header field needs to be
        // accumulated. The header is updated once in `finalize`.
    }

    /// Finalizes the coalesced IPv6 packet by updating the header with the new
    /// payload length.
    fn finalize(&self, mut ip_view: &mut [u8]) {
        let payload_len = ip_view.len() - IPV6_FIXED_HDR_LEN;
        let mut ip = Ipv6PacketRaw::parse_mut(&mut ip_view, ()).expect("valid IPv6 header");
        let payload_len_u16 = u16::try_from(payload_len).expect("IPv6 payload len fits in u16");
        ip.set_payload_len(payload_len_u16);
    }
}

/// IP-specific flow state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IpFlow {
    Ipv4(Ipv4Flow),
    Ipv6(Ipv6Flow),
}

impl IpFlow {
    fn new(ip: &IpPacket<'_>) -> Self {
        match ip {
            IpPacket::V4(packet) => IpFlow::Ipv4(Ipv4Flow::new(packet)),
            IpPacket::V6(packet) => IpFlow::Ipv6(Ipv6Flow::new(packet)),
        }
    }

    fn can_coalesce(&self, ip: &IpPacket<'_>) -> bool {
        match (self, ip) {
            (Self::Ipv4(flow), IpPacket::V4(packet)) => flow.can_coalesce(packet),
            (Self::Ipv6(flow), IpPacket::V6(packet)) => flow.can_coalesce(packet),
            (Self::Ipv4(_), IpPacket::V6(_)) | (Self::Ipv6(_), IpPacket::V4(_)) => false,
        }
    }

    fn coalesce(&mut self, ip: &IpPacket<'_>) {
        match (self, ip) {
            (Self::Ipv4(flow), IpPacket::V4(packet)) => flow.coalesce(packet),
            (Self::Ipv6(flow), IpPacket::V6(packet)) => flow.coalesce(packet),
            (Self::Ipv4(_), IpPacket::V6(_)) | (Self::Ipv6(_), IpPacket::V4(_)) => {
                unreachable!("mismatched IP versions can't be coalesced")
            }
        }
    }

    fn finalize(&self, ip_view: &mut [u8]) {
        match self {
            Self::Ipv4(flow) => flow.finalize(ip_view),
            Self::Ipv6(flow) => flow.finalize(ip_view),
        }
    }

    fn ipv4_id_mode(&self) -> Option<Ipv4IdMode> {
        match self {
            Self::Ipv4(flow) => Some(flow.ipv4_id_mode()),
            Self::Ipv6(_) => None,
        }
    }
}

struct GroFlow<T> {
    target: T,
    flow_id: GroFlowId,
    offsets: HeaderOffsets,
    ip: IpFlow,
    transport: TransportFlow,
    checksum_offload: ChecksumRxOffloading,
    num_coalesced: usize,
}

impl<T: Eq> GroFlow<T> {
    fn new(target: T, parsed: GroPacket<'_>, checksum_offload: ChecksumRxOffloading) -> Self {
        let GroPacket { flow_id, offsets, ip, transport } = parsed;
        Self {
            target,
            flow_id,
            offsets,
            ip: IpFlow::new(&ip),
            transport: TransportFlow::new(&transport),
            checksum_offload,
            num_coalesced: 1,
        }
    }

    fn matches(
        &self,
        target: &T,
        flow_id: &GroFlowId,
        checksum_offload: ChecksumRxOffloading,
    ) -> bool {
        self.target == *target
            && self.flow_id == *flow_id
            && self.checksum_offload == checksum_offload
    }

    fn can_coalesce(&self, current_frame_len: usize, parsed: &GroPacket<'_>) -> bool {
        self.ip.can_coalesce(&parsed.ip)
            && self.transport.can_coalesce(current_frame_len, &parsed.transport)
    }

    fn coalesce(&mut self, parsed: GroPacket<'_>, coalesce_into: &mut Vec<u8>) -> CoalesceResult {
        self.num_coalesced += 1;
        self.ip.coalesce(&parsed.ip);
        self.transport.coalesce(&self.flow_id.ip, &parsed.transport, coalesce_into)
    }

    fn finalize(&self, view: &mut [u8]) -> Option<GsoInfo> {
        let HeaderOffsets { ip_offset, transport_offset } = self.offsets;
        if self.num_coalesced > 1 {
            self.ip.finalize(&mut view[ip_offset..]);
            let gso_size = self.transport.finalize(&mut view[transport_offset..]);
            let ipv4_id_mode = self.ip.ipv4_id_mode();
            Some(GsoInfo { gso_size, ipv4_id_mode })
        } else {
            None
        }
    }
}

/// An input buffer item for GRO processing.
#[derive(Debug, PartialEq, Eq)]
pub struct GroInputItem<B, T> {
    /// The buffer.
    pub buffer: B,
    /// Target for the incoming frame.
    pub target: T,
    /// Checksum offload state for the incoming frame.
    pub checksum_offload: ChecksumRxOffloading,
}

/// Buffers associated with a GRO output item.
#[derive(Debug)]
pub enum GroOutputBuffers<'a, B, O> {
    /// A single contiguous buffer.
    Contiguous(B),
    /// A single buffer that was linearized into temporary scratch space.
    Linearized {
        /// The linearized slice view into scratch storage.
        slice: &'a mut [u8],
        /// The original buffer.
        buffer: B,
    },
    /// A coalesced set of buffers.
    Coalesced {
        /// The coalesced slice view into coalescing storage.
        slice: &'a mut [u8],
        /// The original buffers that formed this frame.
        buffers: O,
    },
}

impl<'a, B: MaybeContiguousBuffer, O> GroOutputBuffers<'a, B, O> {
    /// Returns a mutable slice view of the frame buffer.
    pub fn slice_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Contiguous(b) => b.unwrap_contiguous(),
            Self::Linearized { slice, .. } | Self::Coalesced { slice, .. } => slice,
        }
    }
}

/// An item yielded by GRO processing.
#[derive(Debug)]
pub struct GroOutputItem<'a, B, T, O> {
    /// Target for the frame.
    pub target: T,
    /// Checksum offload state for the frame.
    pub checksum_offload: ChecksumRxOffloading,
    /// GSO metadata if the frame was coalesced from multiple segments.
    pub gso_info: Option<GsoInfo>,
    /// The buffer(s) associated with this frame.
    pub buffers: GroOutputBuffers<'a, B, O>,
}

/// Persistent reusable buffer storage for GRO to save on per-batch allocations.
#[derive(Debug, Derivative)]
#[derivative(Default(bound = ""))]
pub struct GroBufferStorage<B> {
    /// Buffer for GRO coalescing.
    coalescing_vec: Vec<u8>,
    /// Holds onto the original buffers while building a coalesced frame before
    /// it's passed to the stack.
    coalesced_buffers: Vec<B>,
    /// Buffer for linearization of fragmented buffers.
    linearization_vec: Vec<u8>,
}

impl<B> GroBufferStorage<B> {
    /// Creates a new `GroBufferStorage`.
    pub fn new() -> Self {
        Self::default()
    }

    fn clear(&mut self) {
        self.coalescing_vec.clear();
        self.linearization_vec.clear();
        self.coalesced_buffers.clear();
    }
}

impl<B: MaybeContiguousBuffer> GroBufferStorage<B> {
    /// Adapts the provided iterator of packet buffers into a GRO iterator.
    pub fn coalesce<I, T>(&mut self, iter: I, enable_tcp_gro: bool) -> GroIter<'_, I, B, T>
    where
        I: Iterator<Item = GroInputItem<B, T>>,
        T: GroBufferDestination,
    {
        let enable_tcp_gro = match iter.size_hint() {
            (_, Some(1)) => false,
            _ => enable_tcp_gro,
        };
        GroIter::new(iter, self, enable_tcp_gro)
    }

    fn build_output<'a, T: Eq>(
        &'a mut self,
        ActiveFlow { flow, buffers }: ActiveFlow<B, T>,
    ) -> GroOutputItem<'a, B, T, alloc::vec::Drain<'a, B>> {
        match buffers {
            // Nothing was ever merged into this flow, so its buffer was never
            // copied into the coalescing buffer; hand it back untouched.
            ActiveFlowBuffers::Single { buffer, payload_end: _ } => GroOutputItem {
                target: flow.target,
                checksum_offload: flow.checksum_offload,
                gso_info: None,
                buffers: GroOutputBuffers::Contiguous(buffer),
            },
            ActiveFlowBuffers::Coalesced => {
                let GroBufferStorage { coalescing_vec, coalesced_buffers, .. } = self;

                let gso_info = flow.finalize(&mut coalescing_vec[..]);

                let GroFlow { target, checksum_offload, num_coalesced, .. } = flow;
                let buffers = GroOutputBuffers::Coalesced {
                    slice: &mut coalescing_vec[..],
                    buffers: coalesced_buffers.drain(..num_coalesced),
                };
                GroOutputItem { target, checksum_offload, gso_info, buffers }
            }
        }
    }
}

/// The buffers held by an [`ActiveFlow`].
enum ActiveFlowBuffers<B> {
    /// Nothing has been merged into the flow yet, so its seed buffer is still
    /// held verbatim and has not been copied into the coalescing buffer.
    Single {
        buffer: B,
        /// The seed frame's [`GroPacket::payload_end`]; only the bytes before
        /// this point are copied into the coalescing buffer, since any trailing
        /// bytes would be stranded in the middle of the coalesced payload.
        payload_end: usize,
    },
    /// The flow's frame lives in the coalescing buffer and the buffers that
    /// formed it are held by [`GroBufferStorage`].
    Coalesced,
}

/// A [`GroFlow`] along with the buffers it currently holds.
struct ActiveFlow<B, T> {
    flow: GroFlow<T>,
    buffers: ActiveFlowBuffers<B>,
}

impl<B, T> ActiveFlow<B, T> {
    /// The length the frame for this flow would have if a payload were merged
    /// into it right now.
    fn frame_len(&self, coalescing_vec_len: usize) -> usize {
        match &self.buffers {
            // Nothing has been merged in yet, so the buffer still holds the
            // seed frame verbatim. Any trailing bytes it carries are dropped on
            // the first merge, so they don't count towards the frame length.
            ActiveFlowBuffers::Single { payload_end, .. } => *payload_end,
            ActiveFlowBuffers::Coalesced => coalescing_vec_len,
        }
    }
}

impl<B: MaybeContiguousBuffer, T: Eq> ActiveFlow<B, T> {
    fn matches(
        &self,
        target: &T,
        flow_id: &GroFlowId,
        checksum_offload: ChecksumRxOffloading,
    ) -> bool {
        self.flow.matches(target, flow_id, checksum_offload)
    }

    fn can_coalesce(&self, coalescing_vec_len: usize, parsed: &GroPacket<'_>) -> bool {
        self.flow.can_coalesce(self.frame_len(coalescing_vec_len), parsed)
    }

    /// Merges `parsed` into the flow's frame in `coalesce_into`.
    ///
    /// If the flow is still holding its seed buffer verbatim, the seed is first
    /// copied into `coalesce_into` and the buffer is moved into `hold_buffers`,
    /// which must keep the buffers alive until the frame is handed to the
    /// stack.
    fn coalesce(
        &mut self,
        parsed: GroPacket<'_>,
        coalesce_into: &mut Vec<u8>,
        hold_buffers: &mut Vec<B>,
    ) -> CoalesceResult {
        match core::mem::replace(&mut self.buffers, ActiveFlowBuffers::Coalesced) {
            ActiveFlowBuffers::Single { mut buffer, payload_end } => {
                coalesce_into.clear();
                // Unwrapping is okay here because `ActiveFlowBuffers::Single`
                // is only ever constructed for a buffer that was found to be
                // contiguous; a fragmented buffer is linearized into the
                // coalescing buffer up front and recorded as
                // `ActiveFlowBuffers::Coalesced`.
                //
                // Any trailing bytes (e.g. link layer padding) are dropped;
                // only the packet itself may be coalesced against.
                coalesce_into.extend_from_slice(&buffer.unwrap_contiguous()[..payload_end]);
                hold_buffers.push(buffer);
            }
            ActiveFlowBuffers::Coalesced => {}
        }
        self.flow.coalesce(parsed, coalesce_into)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BufferLinearization {
    Contiguous,
    Linearized,
}

/// The subsequent call to `GroIter::next()` must establish this as the active
/// flow.
///
/// To avoid double-linearizing a fragmented buffer, `linearization` indicates
/// whether `buffer` is contiguous or its linearization is still held in the
/// linearization buffer.
struct PendingFlow<B, T> {
    buffer: B,
    flow: GroFlow<T>,
    /// The seed frame's [`GroPacket::payload_end`]; only the bytes before this
    /// point are copied into the coalescing buffer, since any trailing bytes
    /// would be stranded in the middle of the coalesced payload.
    seed_payload_end: usize,
    linearization: BufferLinearization,
}

/// The subsequent call to `GroIter::next()` must flush this buffer.
///
/// To avoid double-linearizing a fragmented buffer, `linearization` indicates
/// whether `buffer` is contiguous or its linearization is still held in the
/// linearization buffer.
struct PendingFlush<B, T> {
    buffer: B,
    target: T,
    linearization: BufferLinearization,
    checksum_offload: ChecksumRxOffloading,
}

enum PendingItem<B, T> {
    NewFlow(PendingFlow<B, T>),
    Flush(PendingFlush<B, T>),
}

/// An iterator adapter for GRO processing.
pub struct GroIter<'a, I, B, T> {
    iter: I,
    storage: &'a mut GroBufferStorage<B>,
    /// The active GRO flow that GRO is attempting to match.
    active_flow: Option<ActiveFlow<B, T>>,
    /// Item pending processing on next iteration.
    pending_item: Option<PendingItem<B, T>>,
    enable_tcp_gro: bool,
}

impl<'a, I, B, T> GroIter<'a, I, B, T>
where
    B: MaybeContiguousBuffer,
{
    fn new(iter: I, storage: &'a mut GroBufferStorage<B>, enable_tcp_gro: bool) -> Self {
        Self { iter, storage, active_flow: None, pending_item: None, enable_tcp_gro }
    }
}

impl<'a, I, B, T> Drop for GroIter<'a, I, B, T> {
    fn drop(&mut self) {
        self.storage.clear();
    }
}

enum ProcessingResult<'a, B, T, O> {
    Continue,
    Return(GroOutputItem<'a, B, T, O>),
}

impl<'a, I, B, T> GroIter<'a, I, B, T>
where
    B: MaybeContiguousBuffer,
    T: GroBufferDestination,
    I: Iterator<Item = GroInputItem<B, T>>,
{
    /// Advances the iterator and returns the next GRO output item.
    pub fn next<'b>(&'b mut self) -> Option<GroOutputItem<'b, B, T, alloc::vec::Drain<'b, B>>> {
        loop {
            if let Some(i) = self.pending_item.take() {
                match self.process_pending(i) {
                    ProcessingResult::Continue => continue,
                    ProcessingResult::Return(out) => return Some(out),
                };
            }
            if let Some(i) = self.iter.next() {
                match self.process_input(i) {
                    ProcessingResult::Continue => continue,
                    ProcessingResult::Return(out) => return Some(out),
                };
            }
            if let Some(f) = self.active_flow.take() {
                return Some(self.storage.build_output(f));
            }
            return None;
        }
    }

    fn process_pending<'b>(
        &'b mut self,
        pending: PendingItem<B, T>,
    ) -> ProcessingResult<'b, B, T, alloc::vec::Drain<'b, B>> {
        let Self { storage, active_flow, .. } = self;
        match pending {
            PendingItem::NewFlow(pending) => {
                let PendingFlow { buffer, flow, seed_payload_end, linearization } = pending;
                let buffers = match linearization {
                    // The buffer is contiguous, so the flow can hold onto it
                    // verbatim and defer copying it into the coalescing buffer
                    // until something merges into the flow.
                    BufferLinearization::Contiguous => {
                        ActiveFlowBuffers::Single { buffer, payload_end: seed_payload_end }
                    }
                    BufferLinearization::Linearized => {
                        // The buffer is fragmented and its linearization is
                        // already in the linearization buffer; move it into the
                        // coalescing buffer, which the previous active flow has
                        // now released.
                        //
                        // Drop any trailing bytes (e.g. link layer padding)
                        // from the seed frame; only the packet itself may be
                        // coalesced against.
                        let GroBufferStorage {
                            coalescing_vec,
                            coalesced_buffers,
                            linearization_vec,
                        } = storage;
                        coalescing_vec.clear();
                        coalescing_vec.extend_from_slice(&linearization_vec[..seed_payload_end]);
                        coalesced_buffers.push(buffer);
                        ActiveFlowBuffers::Coalesced
                    }
                };

                *active_flow = Some(ActiveFlow { flow, buffers });
                ProcessingResult::Continue
            }
            PendingItem::Flush(pending) => {
                let PendingFlush { buffer, target, linearization, checksum_offload } = pending;

                let buffers = match linearization {
                    BufferLinearization::Contiguous => GroOutputBuffers::Contiguous(buffer),
                    BufferLinearization::Linearized => GroOutputBuffers::Linearized {
                        buffer,
                        slice: &mut storage.linearization_vec,
                    },
                };
                ProcessingResult::Return(GroOutputItem {
                    target,
                    checksum_offload,
                    gso_info: None,
                    buffers,
                })
            }
        }
    }

    /// Processes a GRO input item and returns the action to be taken as a
    /// result of the processing.
    fn process_input<'b>(
        &'b mut self,
        item: GroInputItem<B, T>,
    ) -> ProcessingResult<'b, B, T, alloc::vec::Drain<'b, B>> {
        let Self { storage, active_flow, pending_item, enable_tcp_gro, .. } = self;
        let GroInputItem { mut buffer, target, checksum_offload } = item;

        storage.linearization_vec.clear();
        let buffer_slice = buffer
            .linearized(Some(&mut storage.linearization_vec))
            .expect("must be `Some` if linearization vec is provided");
        let linearization = match &buffer_slice {
            BufferSlice::Contiguous(_) => BufferLinearization::Contiguous,
            BufferSlice::Linearized(_) => BufferLinearization::Linearized,
        };

        // Implemented as a macro rather than a function or closure because
        // passing `buffer` and `buffer_slice` across a call boundary causes the
        // borrow checker to reject moving `buffer` while it is borrowed by
        // `buffer_slice`. Expanding the pattern match inline allows the borrow
        // to be dropped before moving `buffer` in the `Contiguous` arm.
        macro_rules! return_single_buffer {
            ($csum_offload:expr) => {{
                let buffers = match buffer_slice {
                    BufferSlice::Contiguous(_) => GroOutputBuffers::Contiguous(buffer),
                    BufferSlice::Linearized(slice) => {
                        GroOutputBuffers::Linearized { buffer, slice }
                    }
                };
                return ProcessingResult::Return(GroOutputItem {
                    target,
                    checksum_offload: $csum_offload,
                    gso_info: None,
                    buffers,
                });
            }};
        }

        if !*enable_tcp_gro {
            return_single_buffer!(checksum_offload);
        }

        let mut context = NetworkParsingContext::new(checksum_offload);
        let parsed = GroPacket::parse(buffer_slice.as_slice(), &target, &mut context);
        // Note: even if `parse` failed to produce a GRO-eligible packet, it may
        // still have verified the transport checksum so we build the output
        // offloading indication prior to checking the parse result.
        let checksum_offload = build_csum_offload_output(checksum_offload, &context);
        let parsed = match parsed {
            Some(p) => p,
            None => return_single_buffer!(checksum_offload),
        };

        let flush_if_not_merged = parsed.flush_if_not_merged(buffer_slice.as_slice().len());
        let Some(mut active) = active_flow.take() else {
            // There's no active flow, so the buffer was not merged. Establish
            // it as the active flow unless it must be flushed immediately.

            if flush_if_not_merged {
                return_single_buffer!(checksum_offload);
            }

            let seed_payload_end = parsed.payload_end();
            let flow = GroFlow::new(target, parsed, checksum_offload);
            let buffers = match linearization {
                // The buffer is contiguous, so the flow can hold onto it
                // verbatim and defer copying it into the coalescing buffer
                // until something merges into the flow.
                BufferLinearization::Contiguous => {
                    ActiveFlowBuffers::Single { buffer, payload_end: seed_payload_end }
                }
                BufferLinearization::Linearized => {
                    storage.coalescing_vec.clear();
                    // Drop any trailing bytes (e.g. link layer padding) from
                    // the seed frame; only the packet itself may be coalesced
                    // against.
                    storage
                        .coalescing_vec
                        .extend_from_slice(&buffer_slice.as_slice()[..seed_payload_end]);
                    storage.coalesced_buffers.push(buffer);
                    ActiveFlowBuffers::Coalesced
                }
            };
            *active_flow = Some(ActiveFlow { flow, buffers });
            return ProcessingResult::Continue;
        };

        if !active.matches(&target, &parsed.flow_id, checksum_offload) {
            // The buffer didn't match the active flow, so it was not merged and
            // the active flow is not flushed. Replace the active flow with a
            // new one unless the buffer must be flushed immediately, in which
            // case we just keep the active flow as-is.
            //
            // Note that while this may result in packet re-ordering across
            // separate flows, it will still preserve ordering within any given
            // flow. This is consistent with Linux's implementation of GRO
            // (albeit with a single tracked flow, where Linux tracks up to 8).

            if flush_if_not_merged {
                *active_flow = Some(active);
                return_single_buffer!(checksum_offload);
            }

            let seed_payload_end = parsed.payload_end();
            let flow = GroFlow::new(target, parsed, checksum_offload);
            let pending =
                PendingItem::NewFlow(PendingFlow { buffer, flow, seed_payload_end, linearization });
            *pending_item = Some(pending);
            return ProcessingResult::Return(storage.build_output(active));
        }

        if active.can_coalesce(storage.coalescing_vec.len(), &parsed) {
            let coalesce_result = active.coalesce(
                parsed,
                &mut storage.coalescing_vec,
                &mut storage.coalesced_buffers,
            );

            storage.coalesced_buffers.push(buffer);

            match coalesce_result {
                CoalesceResult::Flush => ProcessingResult::Return(storage.build_output(active)),
                CoalesceResult::Continue => {
                    *active_flow = Some(active);
                    ProcessingResult::Continue
                }
            }
        } else {
            // The buffer matched the active flow but could not be merged into
            // it, so the active flow is flushed. The buffer is flushed as well
            // if it must be; otherwise it becomes the new active flow.

            let pending = if flush_if_not_merged {
                PendingItem::Flush(PendingFlush { buffer, target, linearization, checksum_offload })
            } else {
                let seed_payload_end = parsed.payload_end();
                let flow = GroFlow::new(target, parsed, checksum_offload);
                PendingItem::NewFlow(PendingFlow { buffer, flow, seed_payload_end, linearization })
            };
            *pending_item = Some(pending);
            ProcessingResult::Return(storage.build_output(active))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::vec;
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use net_declare::{net_ip_v4, net_ip_v6, net_mac};
    use net_types::ip::{Ipv4, Ipv6};
    use netstack3_base::NetworkSerializationContext;
    use packet::{Buf, InnerPacketBuilder, NestableSerializer as _, Serializer};
    use packet_formats::arp::{ArpOp, ArpPacketBuilder};
    use packet_formats::ethernet::{EtherType, EthernetFrameBuilder};
    use packet_formats::ip::{FragmentOffset, IpExt, IpProto, Ipv4Proto};
    use packet_formats::ipv4::options::Ipv4Option;
    use packet_formats::ipv4::{Ipv4PacketBuilder, Ipv4PacketBuilderWithOptions};
    use packet_formats::ipv6::ext_hdrs::{
        ExtensionHeaderOptionAction, HopByHopOption, HopByHopOptionData,
    };
    use packet_formats::ipv6::{Ipv6PacketBuilder, Ipv6PacketBuilderWithHbhOptions};
    use packet_formats::tcp::options::{TcpOptionsBuilder, TimestampOption};
    use packet_formats::tcp::{TcpSegmentBuilder, TcpSegmentBuilderWithOptions};
    use packet_formats::udp::UdpPacketBuilder;
    use test_case::test_case;

    impl GroBufferDestination for GroFrameType {
        fn frame_type(&self) -> GroFrameType {
            *self
        }
    }

    /// A buffer that records how GRO used it.
    #[derive(Debug)]
    struct TrackedBuffer {
        buf: Vec<u8>,
        contiguous: bool,
        dropped: Arc<AtomicBool>,
        linearized_count: Arc<AtomicUsize>,
    }

    impl TrackedBuffer {
        fn new(buf: Vec<u8>, contiguous: bool) -> Self {
            Self {
                buf,
                contiguous,
                dropped: Arc::new(AtomicBool::new(false)),
                linearized_count: Arc::new(AtomicUsize::new(0)),
            }
        }

        /// Returns a handle reporting whether this buffer has been dropped.
        fn dropped(&self) -> Arc<AtomicBool> {
            self.dropped.clone()
        }

        /// Returns a handle to the number of times this buffer has been
        /// linearized.
        fn linearized_count(&self) -> Arc<AtomicUsize> {
            self.linearized_count.clone()
        }
    }

    impl Drop for TrackedBuffer {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    impl MaybeContiguousBuffer for TrackedBuffer {
        fn linearized<'a, 'b>(
            &'a mut self,
            storage: Option<&'b mut Vec<u8>>,
        ) -> Option<BufferSlice<'a, 'b>> {
            if self.contiguous {
                Some(BufferSlice::Contiguous(&mut self.buf[..]))
            } else {
                let storage = storage?;
                let _: usize = self.linearized_count.fetch_add(1, Ordering::SeqCst);
                let frame_length = self.buf.len();
                if storage.len() < frame_length {
                    storage.resize(frame_length, 0);
                }
                let slice = &mut storage[..frame_length];
                slice.copy_from_slice(&self.buf);
                Some(BufferSlice::Linearized(slice))
            }
        }
    }

    /// Wraps `buffer` in an Ethernet GRO input item.
    fn input_item(buffer: TrackedBuffer) -> GroInputItem<TrackedBuffer, GroFrameType> {
        GroInputItem {
            buffer,
            target: GroFrameType::Ethernet,
            checksum_offload: ChecksumRxOffloading::FullyOffloaded,
        }
    }

    #[test]
    fn process_gro_handles_fragmented() {
        let items: Vec<GroInputItem<TrackedBuffer, GroFrameType>> = vec![
            GroInputItem {
                buffer: TrackedBuffer::new(vec![1, 2, 3], true),
                target: GroFrameType::Ethernet,
                checksum_offload: ChecksumRxOffloading::FullyOffloaded,
            },
            GroInputItem {
                buffer: TrackedBuffer::new(vec![4, 5, 6], false),
                target: GroFrameType::Ethernet,
                checksum_offload: ChecksumRxOffloading::FullyOffloaded,
            },
        ];

        let mut storage = GroBufferStorage::new();
        let mut output = Vec::new();
        let mut gro = storage.coalesce(items.into_iter(), false);
        while let Some(mut item) = gro.next() {
            output.push(item.buffers.slice_mut().to_vec());
        }

        assert_eq!(output, vec![vec![1, 2, 3], vec![4, 5, 6]]);
    }

    const SRC_MAC: Mac = net_mac!("00:11:22:33:44:55");
    const DST_MAC: Mac = net_mac!("66:77:88:99:aa:bb");
    const SRC_IP_V4: Ipv4Addr = net_ip_v4!("192.168.0.1");
    const DST_IP_V4: Ipv4Addr = net_ip_v4!("192.168.0.2");
    const SRC_IP_V6: Ipv6Addr = net_ip_v6!("2001:db8::1");
    const DST_IP_V6: Ipv6Addr = net_ip_v6!("2001:db8::2");

    /// A TCP-over-IP frame to feed to GRO.
    ///
    /// Construct with [`v4`] or [`v6`] and override the fields of interest with
    /// struct update syntax, e.g. `FrameSpec { psh: true, ..v4(100, b"x") }`.
    #[derive(Clone, Debug)]
    struct FrameSpec {
        /// The IP version to emit, along with its version-specific fields.
        ip: IpSpec,
        seq: u32,
        ack: u32,
        win: u16,
        psh: bool,
        fin: bool,
        syn: bool,
        rst: bool,
        urg: bool,
        /// The TTL for IPv4, or the hop limit for IPv6.
        ttl: u8,
        dscp_and_ecn: DscpAndEcn,
        /// Emits a TCP timestamp option with this TSval when set.
        timestamp: Option<u32>,
        /// The minimum Ethernet body length, used to force link layer padding.
        min_body_len: usize,
        payload: &'static [u8],
        /// Whether the frame is presented to GRO as a contiguous buffer.
        contiguous: bool,
    }

    /// The IP-version-specific fields of a [`FrameSpec`].
    #[derive(Clone, Copy, Debug)]
    enum IpSpec {
        V4 { df: bool, id: u16 },
        V6 { flowlabel: u32 },
    }

    /// Returns a default IPv4 frame carrying `payload` at `seq`.
    fn v4(seq: u32, payload: &'static [u8]) -> FrameSpec {
        FrameSpec {
            ip: IpSpec::V4 { df: false, id: 0 },
            seq,
            ack: 1000,
            win: 64240,
            psh: false,
            fin: false,
            syn: false,
            rst: false,
            urg: false,
            ttl: 64,
            dscp_and_ecn: DscpAndEcn::default(),
            timestamp: None,
            min_body_len: 0,
            payload,
            contiguous: true,
        }
    }

    /// Returns a default IPv6 frame carrying `payload` at `seq`.
    fn v6(seq: u32, payload: &'static [u8]) -> FrameSpec {
        FrameSpec { ip: IpSpec::V6 { flowlabel: 0 }, ..v4(seq, payload) }
    }

    impl FrameSpec {
        /// Serializes this spec into an Ethernet frame.
        fn build(&self) -> Vec<u8> {
            let FrameSpec {
                ip: ip_spec,
                seq,
                ack,
                win,
                psh,
                fin,
                syn,
                rst,
                urg,
                ttl,
                dscp_and_ecn,
                timestamp,
                min_body_len,
                payload,
                contiguous: _,
            } = *self;

            let mut body = payload.to_vec();

            macro_rules! tcp_builder {
                ($src:expr, $dst:expr) => {{
                    let mut tcp = TcpSegmentBuilder::new(
                        $src,
                        $dst,
                        TEST_SRC_PORT,
                        TEST_DST_PORT,
                        seq,
                        Some(ack),
                        win,
                    );
                    tcp.psh(psh);
                    tcp.fin(fin);
                    tcp.syn(syn);
                    tcp.rst(rst);
                    tcp.urg(urg);
                    tcp
                }};
            }

            macro_rules! serialize {
                ($tcp:expr, $ip:expr, $ethertype:expr) => {
                    Buf::new(&mut body[..], ..)
                        .wrap_in($tcp)
                        .wrap_in($ip)
                        .wrap_in(EthernetFrameBuilder::new(
                            SRC_MAC,
                            DST_MAC,
                            $ethertype,
                            min_body_len,
                        ))
                        .serialize_vec_outer(&mut NetworkSerializationContext::default())
                        .unwrap()
                        .unwrap_b()
                        .as_ref()
                        .to_vec()
                };
            }

            // NB: the TCP builder type differs depending on whether options are
            // present, so each combination needs its own serialization.
            macro_rules! serialize_with_options {
                ($tcp:expr, $ip:expr, $ethertype:expr) => {
                    match timestamp {
                        None => serialize!($tcp, $ip, $ethertype),
                        Some(ts_val) => {
                            let options = TcpOptionsBuilder {
                                timestamp: Some(TimestampOption::new(ts_val, 0)),
                                ..Default::default()
                            };
                            let tcp = TcpSegmentBuilderWithOptions::new($tcp, options).unwrap();
                            serialize!(tcp, $ip, $ethertype)
                        }
                    }
                };
            }

            match ip_spec {
                IpSpec::V4 { df, id } => {
                    let mut ip =
                        Ipv4PacketBuilder::new(SRC_IP_V4, DST_IP_V4, ttl, IpProto::Tcp.into());
                    ip.dscp_and_ecn(dscp_and_ecn);
                    ip.df_flag(df);
                    ip.id(id);
                    serialize_with_options!(tcp_builder!(SRC_IP_V4, DST_IP_V4), ip, EtherType::Ipv4)
                }
                IpSpec::V6 { flowlabel } => {
                    let mut ip =
                        Ipv6PacketBuilder::new(SRC_IP_V6, DST_IP_V6, ttl, IpProto::Tcp.into());
                    ip.dscp_and_ecn(dscp_and_ecn);
                    ip.flowlabel(flowlabel);
                    serialize_with_options!(tcp_builder!(SRC_IP_V6, DST_IP_V6), ip, EtherType::Ipv6)
                }
            }
        }
    }

    /// Runs GRO over `frames` and returns the frames it emits.
    fn run_gro(frames: &[FrameSpec], enable_tcp_gro: bool) -> Vec<Vec<u8>> {
        let items: Vec<GroInputItem<TrackedBuffer, GroFrameType>> = frames
            .iter()
            .map(|spec| input_item(TrackedBuffer::new(spec.build(), spec.contiguous)))
            .collect();

        let mut storage = GroBufferStorage::new();
        let mut output = Vec::new();
        let mut gro = storage.coalesce(items.into_iter(), enable_tcp_gro);
        while let Some(mut item) = gro.next() {
            output.push(item.buffers.slice_mut().to_vec());
        }
        output
    }

    /// Parses `frame` as a GRO packet, verifying its checksum.
    #[track_caller]
    fn parse_frame(frame: &[u8]) -> GroPacket<'_> {
        GroPacket::parse(
            frame,
            &GroFrameType::Ethernet,
            &mut NetworkParsingContext::new(ChecksumRxOffloading::Offloaded(None)),
        )
        .expect("frame is a valid GRO packet")
    }

    #[test_case(
        vec![v4(100, b"hello "), v4(106, b"world")],
        vec![b"hello world"]; "coalesces_contiguous")]
    #[test_case(
        vec![
            FrameSpec { contiguous: false, ..v4(100, b"foo ") },
            FrameSpec { contiguous: false, ..v4(104, b"bar") },
        ],
        vec![b"foo bar"]; "coalesces_fragmented")]
    #[test_case(
        vec![v4(100, b"contig "), FrameSpec { contiguous: false, ..v4(107, b"frag") }],
        vec![b"contig frag"]; "coalesces_contiguous_then_fragmented")]
    #[test_case(
        vec![FrameSpec { contiguous: false, ..v4(100, b"single") }],
        vec![b"single"]; "lone_fragmented_frame_is_emitted")]
    #[test_case(
        vec![v4(100, b"first "), v4(106, b"second"), v4(112, b"third")],
        vec![b"first secondthird"]; "coalesces_three_frames")]
    #[test_case(
        vec![v6(200, b"ipv6_1"), FrameSpec { psh: true, ..v6(206, b"ipv6_2") }],
        vec![b"ipv6_1ipv6_2"]; "coalesces_ipv6")]
    #[test_case(
        vec![
            FrameSpec { ack: 1000, ..v4(100, b"ack1000 ") },
            FrameSpec { ack: 2000, ..v4(108, b"ack2000") },
        ],
        vec![b"ack1000 ", b"ack2000"]; "ack_mismatch_flushes")]
    #[test_case(
        vec![
            FrameSpec { win: 64240, ..v4(100, b"win64k ") },
            FrameSpec { win: 32120, ..v4(107, b"win32k") },
        ],
        vec![b"win64k ", b"win32k"]; "window_mismatch_flushes")]
    #[test_case(
        vec![
            FrameSpec { timestamp: Some(1), ..v4(100, b"opt1") },
            FrameSpec { timestamp: Some(1), ..v4(104, b"opt2") },
        ],
        vec![b"opt1opt2"]; "matching_options_coalesce")]
    #[test_case(
        vec![
            FrameSpec { timestamp: Some(1), ..v4(100, b"opt1") },
            FrameSpec { timestamp: Some(2), ..v4(104, b"mismatch") },
        ],
        vec![b"opt1", b"mismatch"]; "options_mismatch_flushes")]
    #[test_case(
        vec![
            FrameSpec { ttl: 64, ..v4(100, b"ttl64 ") },
            FrameSpec { ttl: 63, ..v4(106, b"ttl63") },
        ],
        vec![b"ttl64 ", b"ttl63"]; "ttl_mismatch_flushes")]
    #[test_case(
        vec![
            FrameSpec { ip: IpSpec::V4 { df: true, id: 0 }, ..v4(100, b"df1 ") },
            FrameSpec { ip: IpSpec::V4 { df: false, id: 0 }, ..v4(104, b"df0") },
        ],
        vec![b"df1 ", b"df0"]; "df_flag_mismatch_flushes")]
    #[test_case(
        vec![
            FrameSpec { dscp_and_ecn: DscpAndEcn::new(1, 0), ..v4(100, b"ecn1 ") },
            FrameSpec { dscp_and_ecn: DscpAndEcn::new(2, 0), ..v4(105, b"ecn2") },
        ],
        vec![b"ecn1 ", b"ecn2"]; "dscp_ecn_mismatch_flushes")]
    #[test_case(
        vec![
            FrameSpec { ttl: 64, ..v6(200, b"hop64 ") },
            FrameSpec { ttl: 63, ..v6(206, b"hop63") },
        ],
        vec![b"hop64 ", b"hop63"]; "hop_limit_mismatch_flushes")]
    #[test_case(
        vec![
            FrameSpec { ip: IpSpec::V6 { flowlabel: 123 }, ..v6(200, b"lbl1 ") },
            FrameSpec { ip: IpSpec::V6 { flowlabel: 456 }, ..v6(205, b"lbl2") },
        ],
        vec![b"lbl1 ", b"lbl2"]; "flowlabel_mismatch_flushes")]
    #[test_case(
        vec![v4(100, b"seq100 "), v4(200, b"seq200")],
        vec![b"seq100 ", b"seq200"]; "out_of_order_seq_flushes")]
    #[test_case(
        vec![v4(100, b"seq100 "), v4(200, b"seq200"), v4(206, b"seq206")],
        vec![b"seq100 ", b"seq200seq206"]; "merge_failure_starts_new_flow")]
    #[test_case(
        vec![
            FrameSpec { urg: true, ..v4(100, b"urg ") },
            FrameSpec { urg: false, ..v4(104, b"normal") },
        ],
        vec![b"urg ", b"normal"]; "urg_flag_flushes")]
    #[test_case(
        vec![
            FrameSpec { urg: true, ..v4(100, b"urg1") },
            FrameSpec { urg: true, ..v4(104, b"urg2") },
        ],
        vec![b"urg1", b"urg2"]; "consecutive_urg_flags_flush")]
    #[test_case(
        vec![
            FrameSpec { syn: true, ..v4(100, b"syn1") },
            FrameSpec { syn: true, ..v4(104, b"syn2") },
        ],
        vec![b"syn1", b"syn2"]; "syn_flag_flushes")]
    #[test_case(
        vec![
            FrameSpec { rst: true, ..v4(100, b"rst1") },
            FrameSpec { rst: true, ..v4(104, b"rst2") },
        ],
        vec![b"rst1", b"rst2"]; "rst_flag_flushes")]
    #[test_case(
        vec![
            FrameSpec { ip: IpSpec::V4 { df: false, id: 42 }, ..v4(100, b"hello ") },
            FrameSpec { ip: IpSpec::V4 { df: false, id: 42 }, ..v4(106, b"world ") },
            FrameSpec { ip: IpSpec::V4 { df: false, id: 42 }, ..v4(112, b"again") },
        ],
        vec![b"hello world again"]; "consistent_ipv4_ids_coalesce")]
    #[test_case(
        vec![
            FrameSpec { ip: IpSpec::V4 { df: false, id: 100 }, ..v4(100, b"hello ") },
            FrameSpec { ip: IpSpec::V4 { df: false, id: 101 }, ..v4(106, b"world ") },
            FrameSpec { ip: IpSpec::V4 { df: false, id: 102 }, ..v4(112, b"again") },
        ],
        vec![b"hello world again"]; "increasing_ipv4_ids_coalesce")]
    #[test_case(
        vec![
            FrameSpec { ip: IpSpec::V4 { df: false, id: u16::MAX }, ..v4(100, b"hello ") },
            FrameSpec { ip: IpSpec::V4 { df: false, id: 0 }, ..v4(106, b"world") },
        ],
        vec![b"hello world"]; "wrapping_ipv4_ids_coalesce")]
    #[test_case(
        vec![
            FrameSpec { ip: IpSpec::V4 { df: false, id: 100 }, ..v4(100, b"hello ") },
            FrameSpec { ip: IpSpec::V4 { df: false, id: 105 }, ..v4(106, b"world") },
        ],
        vec![b"hello ", b"world"]; "ipv4_id_mismatch_flushes")]
    #[test_case(
        vec![
            FrameSpec { ip: IpSpec::V4 { df: false, id: 100 }, ..v4(100, b"first ") },
            FrameSpec { ip: IpSpec::V4 { df: false, id: 100 }, ..v4(106, b"second") },
            FrameSpec { ip: IpSpec::V4 { df: false, id: 101 }, ..v4(112, b"third") },
        ],
        vec![b"first second", b"third"]; "ipv4_id_mode_switch_flushes")]
    #[test_case(
        vec![FrameSpec { min_body_len: 46, ..v4(100, b"abcd") }, v4(104, b"e")],
        vec![b"abcde"]; "padded_seed_frame_is_trimmed")]
    #[test_case(
        vec![
            FrameSpec { min_body_len: 46, contiguous: false, ..v4(100, b"abcd") },
            v4(104, b"e"),
        ],
        vec![b"abcde"]; "linearized_padded_seed_frame_is_trimmed")]
    #[test_case(
        vec![FrameSpec { min_body_len: 46, ..v4(100, b"a") }, v4(101, b"normal")],
        vec![b"a", b"normal"]; "unmerged_padded_frame_is_emitted_verbatim")]
    #[test_case(
        vec![v4(100, b"first "), FrameSpec { min_body_len: 46, ..v4(106, b"b") }],
        vec![b"first b"]; "padded_second_frame_is_trimmed")]
    #[test_case(
        vec![FrameSpec { min_body_len: 70, ..v6(200, b"c") }, v6(201, b"ipv6_normal")],
        vec![b"c", b"ipv6_normal"]; "unmerged_padded_ipv6_frame_is_emitted_verbatim")]
    #[test_case(
        vec![v4(100, b"short"), v4(105, b"longer_payload")],
        vec![b"short", b"longer_payload"]; "payload_larger_than_gso_size_flushes")]
    #[test_case(
        vec![v4(100, b"data"), v4(104, b"")],
        vec![b"data", b""]; "empty_payload_in_active_flow_flushes")]
    #[test_case(
        vec![v4(100, b"odd"), v4(103, b"len"), v4(106, b"tcp")],
        vec![b"oddlentcp"]; "coalesces_odd_length_payloads")]
    #[test_case(
        vec![
            FrameSpec { timestamp: Some(1), ..v4(100, b"odd") },
            FrameSpec { timestamp: Some(1), ..v4(103, b"opt") },
        ],
        vec![b"oddopt"]; "coalesces_odd_length_payloads_with_options")]
    #[test_case(
        vec![v4(100, b"first "), v4(106, b"sub"), v4(109, b"more")],
        vec![b"first sub", b"more"]; "payload_smaller_than_gso_size_terminates_flow")]
    fn gro_coalescing(frames: Vec<FrameSpec>, expected: Vec<&'static [u8]>) {
        let output = run_gro(&frames, true);
        assert_eq!(output.len(), expected.len(), "wrong number of frames emitted");

        for (index, (frame, payload)) in output.iter().zip(expected.iter()).enumerate() {
            let parsed = parse_frame(&frame[..]);
            let TransportPacket::Tcp(tcp) = parsed.transport;
            assert_eq!(tcp.body(), *payload, "frame {index}");
        }
    }

    #[test]
    fn gro_disabled_emits_every_frame_unchanged() {
        let frames = vec![v4(100, b"hello "), v4(106, b"world")];
        let output = run_gro(&frames, false);
        assert_eq!(output, vec![frames[0].build(), frames[1].build()]);
    }

    #[test_case(true; "contiguous")]
    #[test_case(false; "fragmented")]
    fn gro_emits_unmergeable_frames_unmodified(contiguous: bool) {
        // `URG` segments are never coalesced, so every frame is emitted on its
        // own with its headers and payload untouched.
        let frames = vec![
            FrameSpec { urg: true, contiguous, ..v4(100, b"hello ") },
            FrameSpec { urg: true, contiguous, ..v4(106, b"world") },
        ];
        let output = run_gro(&frames, true);
        assert_eq!(output.len(), frames.len(), "wrong number of frames emitted");

        for (index, (frame, spec)) in output.iter().zip(frames.iter()).enumerate() {
            let expected = spec.build();
            let parsed = parse_frame(&expected);
            // Trailing bytes (e.g. link layer padding) may or may not be
            // trimmed, so only compare up to the end of the IP packet.
            let packet_end = parsed.payload_end();
            assert!(frame.len() >= packet_end, "frame {index} is truncated");
            assert_eq!(&frame[..packet_end], &expected[..packet_end], "frame {index}");
        }
    }

    #[test]
    fn gro_merges_psh_into_coalesced_frame() {
        let frames = vec![v4(100, b"hello "), FrameSpec { psh: true, ..v4(106, b"world") }];
        let output = run_gro(&frames, true);
        let output = assert_matches!(&output[..], [output] => output);

        let parsed = parse_frame(output);
        let TransportPacket::Tcp(tcp) = parsed.transport;
        assert_eq!(tcp.body(), b"hello world");
        assert_eq!(tcp.seq_num(), 100);
        assert_eq!(tcp.ack_num(), Some(1000));
        assert_eq!(tcp.window_size(), 64240);
        assert!(tcp.psh());
    }

    #[test]
    fn gro_merges_fin_into_coalesced_frame() {
        let frames = vec![v4(100, b"data "), FrameSpec { fin: true, ..v4(105, b"fin") }];
        let output = run_gro(&frames, true);
        let output = assert_matches!(&output[..], [output] => output);

        let parsed = parse_frame(output);
        let TransportPacket::Tcp(tcp) = parsed.transport;
        assert_eq!(tcp.body(), b"data fin");
        assert!(tcp.fin());
    }

    #[test]
    fn gro_gso_info_metadata() {
        let mut storage = GroBufferStorage::new();

        // Consistent IPv4 IDs across the flow yield a fixed IP ID.
        let items = vec![
            input_item(TrackedBuffer::new(
                FrameSpec { ip: IpSpec::V4 { df: false, id: 1 }, ..v4(100, b"first ") }.build(),
                true,
            )),
            input_item(TrackedBuffer::new(
                FrameSpec { ip: IpSpec::V4 { df: false, id: 1 }, ..v4(106, b"second") }.build(),
                true,
            )),
        ];
        let mut gro = storage.coalesce(items.into_iter(), true);
        let item = gro.next().unwrap();
        assert_eq!(
            item.gso_info,
            Some(GsoInfo {
                gso_size: NonZeroU16::new(6).unwrap(),
                ipv4_id_mode: Some(Ipv4IdMode::Fixed),
            })
        );
        drop(item);
        assert!(gro.next().is_none());
        drop(gro);

        // Increasing IPv4 IDs yield an incrementing IP ID mode.
        let items = vec![
            input_item(TrackedBuffer::new(
                FrameSpec { ip: IpSpec::V4 { df: false, id: 1 }, ..v4(100, b"first ") }.build(),
                true,
            )),
            input_item(TrackedBuffer::new(
                FrameSpec { ip: IpSpec::V4 { df: false, id: 2 }, ..v4(106, b"second") }.build(),
                true,
            )),
        ];
        let mut gro = storage.coalesce(items.into_iter(), true);
        let item = gro.next().unwrap();
        assert_eq!(
            item.gso_info,
            Some(GsoInfo {
                gso_size: NonZeroU16::new(6).unwrap(),
                ipv4_id_mode: Some(Ipv4IdMode::Incrementing),
            })
        );
        drop(item);
        assert!(gro.next().is_none());
        drop(gro);

        // IPv6 flows have no IPv4 ID mode.
        let items = vec![
            input_item(TrackedBuffer::new(v6(100, b"first ").build(), true)),
            input_item(TrackedBuffer::new(v6(106, b"second").build(), true)),
        ];
        let mut gro = storage.coalesce(items.into_iter(), true);
        let item = gro.next().unwrap();
        assert_eq!(
            item.gso_info,
            Some(GsoInfo { gso_size: NonZeroU16::new(6).unwrap(), ipv4_id_mode: None })
        );
        drop(item);
        assert!(gro.next().is_none());
    }

    #[test]
    fn gro_buffers_not_dropped_until_frame_processed() {
        let pkt1 = v4(100, b"hello ").build();
        let pkt2 = v4(106, b"world").build();

        let buffer1 = TrackedBuffer::new(pkt1, true);
        let buffer2 = TrackedBuffer::new(pkt2, true);
        let dropped1 = buffer1.dropped();
        let dropped2 = buffer2.dropped();
        let items = vec![input_item(buffer1), input_item(buffer2)];

        let mut gro_state = GroBufferStorage::new();
        let mut gro = gro_state.coalesce(items.into_iter(), true);

        let item = gro.next();
        assert!(item.is_some());
        let mut item = item.unwrap();
        assert!(item.buffers.slice_mut().ends_with(b"hello world"));

        // While the yielded frame is in use, neither buffer should be dropped!
        assert!(!dropped1.load(Ordering::SeqCst));
        assert!(!dropped2.load(Ordering::SeqCst));

        // When the returned item is dropped, its buffers are dropped.
        drop(item);
        assert!(dropped1.load(Ordering::SeqCst));
        assert!(dropped2.load(Ordering::SeqCst));

        //`GroIter` is a lending iterator so `item` (and by extension any
        // associated buffers) must be dropped before the next call to `next()`.
        let next_item = gro.next();
        assert!(next_item.is_none());
    }

    #[test]
    fn gro_unmerged_flows_are_not_copied() {
        // Nothing merges into either flow, so each hands its buffer back
        // verbatim rather than copying it into the coalescing buffer.
        let first = v4(100, b"one").build();
        // Not sequential with `first`, so it starts a new flow rather than
        // merging into it.
        let second = v4(500, b"two").build();
        let items = vec![
            input_item(TrackedBuffer::new(first.clone(), true)),
            input_item(TrackedBuffer::new(second.clone(), true)),
        ];

        let mut gro_state = GroBufferStorage::new();
        let mut gro = gro_state.coalesce(items.into_iter(), true);

        for expected in [first, second] {
            let mut item = gro.next().expect("emits a frame");
            assert_matches!(&mut item.buffers, GroOutputBuffers::Contiguous(buffer) => {
                assert_eq!(buffer.unwrap_contiguous(), &expected[..]);
            });
        }
        assert!(gro.next().is_none());
    }

    #[test]
    fn gro_iter_drop_releases_held_buffers() {
        let pkt1 = v4(100, b"hello ").build();
        let buffer1 = TrackedBuffer::new(pkt1, true);
        let dropped1 = buffer1.dropped();
        let items = vec![input_item(buffer1)];

        let mut gro_state = GroBufferStorage::new();
        {
            let mut gro = gro_state.coalesce(items.into_iter(), true);
            let item = gro.next();
            assert!(item.is_some());
            assert!(!dropped1.load(Ordering::SeqCst));
        }
        // Dropping `gro` releases all held buffers.
        assert!(dropped1.load(Ordering::SeqCst));
    }

    #[test]
    fn gro_interleaved_non_gro_packet() {
        let pkt1 = v4(100, b"tcp1 ").build();
        let non_gro_pkt = vec![0u8; 30];
        let pkt2 = v4(105, b"tcp2").build();

        let items: Vec<GroInputItem<TrackedBuffer, GroFrameType>> = vec![
            GroInputItem {
                buffer: TrackedBuffer::new(pkt1, true),
                target: GroFrameType::Ethernet,
                checksum_offload: ChecksumRxOffloading::FullyOffloaded,
            },
            GroInputItem {
                buffer: TrackedBuffer::new(non_gro_pkt.clone(), true),
                target: GroFrameType::Ethernet,
                checksum_offload: ChecksumRxOffloading::FullyOffloaded,
            },
            GroInputItem {
                buffer: TrackedBuffer::new(pkt2, true),
                target: GroFrameType::Ethernet,
                checksum_offload: ChecksumRxOffloading::FullyOffloaded,
            },
        ];

        let mut gro_state = GroBufferStorage::new();
        let mut output_frames = Vec::new();
        let mut gro = gro_state.coalesce(items.into_iter(), true);
        while let Some(mut item) = gro.next() {
            output_frames.push(item.buffers.slice_mut().to_vec());
        }

        // Non-GRO frame is emitted immediately without interrupting the TCP GRO flow.
        assert_eq!(output_frames.len(), 2);
        assert_eq!(output_frames[0], non_gro_pkt);
        assert!(output_frames[1].ends_with(b"tcp1 tcp2"));
    }

    #[test]
    fn gro_buffers_linearized_only_once() {
        let buffer1 = TrackedBuffer::new(v4(100, b"first ").build(), false);
        let buffer2 = TrackedBuffer::new(v4(106, b"second").build(), false);
        let buffer3 = TrackedBuffer::new(v4(112, b"third").build(), false);
        let count1 = buffer1.linearized_count();
        let count2 = buffer2.linearized_count();
        let count3 = buffer3.linearized_count();
        let items = vec![input_item(buffer1), input_item(buffer2), input_item(buffer3)];

        let mut gro_state = GroBufferStorage::new();
        let mut output_frames = Vec::new();
        let mut gro = gro_state.coalesce(items.into_iter(), true);
        while let Some(mut item) = gro.next() {
            output_frames.push(item.buffers.slice_mut().to_vec());
        }

        assert_eq!(output_frames.len(), 1);
        assert!(output_frames[0].ends_with(b"first secondthird"));

        assert_eq!(count1.load(Ordering::SeqCst), 1);
        assert_eq!(count2.load(Ordering::SeqCst), 1);
        assert_eq!(count3.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn gro_transport_offset_ignores_padding() {
        // The transport offset must point at the transport header regardless of
        // any trailing link layer padding, which the IP parser strips from the
        // buffer before the transport header is parsed.
        let pkt_padded = FrameSpec { min_body_len: 46, ..v4(100, b"a") }.build();
        let pkt = v4(100, b"a").build();
        assert!(pkt_padded.len() > pkt.len(), "packet should have been padded");

        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        let parsed = GroPacket::parse(&pkt_padded, &GroFrameType::Ethernet, &mut context)
            .expect("should parse padded packet");
        // The IP packet ends where the unpadded frame ends; the padding beyond
        // it is not part of the packet.
        assert_eq!(parsed.offsets.ip_offset + parsed.ip.total_len(), pkt.len());
        let padded_offset = parsed.offsets.transport_offset;

        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        let parsed = GroPacket::parse(&pkt, &GroFrameType::Ethernet, &mut context)
            .expect("should parse unpadded packet");
        assert_eq!(parsed.offsets.ip_offset + parsed.ip.total_len(), pkt.len());
        assert_eq!(padded_offset, parsed.offsets.transport_offset);

        // The offset points at the TCP source and destination ports.
        assert_eq!(&pkt_padded[padded_offset..padded_offset + 2], &1234u16.to_be_bytes());
        assert_eq!(&pkt_padded[padded_offset + 2..padded_offset + 4], &5678u16.to_be_bytes());
    }

    #[test]
    fn gro_buffers_dropped_when_item_dropped() {
        let buffer1 = TrackedBuffer::new(vec![1, 2, 3], true);
        let buffer2 = TrackedBuffer::new(vec![4, 5, 6], false);
        let dropped1 = buffer1.dropped();
        let dropped2 = buffer2.dropped();
        let items = vec![input_item(buffer1), input_item(buffer2)];

        let mut storage = GroBufferStorage::new();
        let mut gro = storage.coalesce(items.into_iter(), false);

        let mut item1 = gro.next().unwrap();
        assert_eq!(item1.buffers.slice_mut(), &[1, 2, 3]);
        assert!(!dropped1.load(Ordering::SeqCst));
        assert!(!dropped2.load(Ordering::SeqCst));
        drop(item1);
        assert!(dropped1.load(Ordering::SeqCst));
        assert!(!dropped2.load(Ordering::SeqCst));

        let mut item2 = gro.next().unwrap();
        assert_eq!(item2.buffers.slice_mut(), &[4, 5, 6]);
        assert!(dropped1.load(Ordering::SeqCst));
        assert!(!dropped2.load(Ordering::SeqCst));
        drop(item2);
        assert!(dropped2.load(Ordering::SeqCst));
    }

    const TEST_SRC_PORT: NonZeroU16 = NonZeroU16::new(1234).unwrap();
    const TEST_DST_PORT: NonZeroU16 = NonZeroU16::new(5678).unwrap();
    const TEST_FLOWLABEL: u32 = 0x12345;
    const TEST_PAYLOAD: [u8; 12] = *b"hello world!";

    trait TestIpExt: IpExt {
        const SRC_IP: Self::Addr;
        const DST_IP: Self::Addr;
        fn ip_builder(proto: IpProto) -> Self::PacketBuilder<NetworkSerializationContext>;
    }

    impl TestIpExt for Ipv4 {
        const SRC_IP: Ipv4Addr = SRC_IP_V4;
        const DST_IP: Ipv4Addr = DST_IP_V4;
        fn ip_builder(proto: IpProto) -> Ipv4PacketBuilder {
            Ipv4PacketBuilder::new(Self::SRC_IP, Self::DST_IP, 64, Ipv4Proto::Proto(proto))
        }
    }

    impl TestIpExt for Ipv6 {
        const SRC_IP: Ipv6Addr = SRC_IP_V6;
        const DST_IP: Ipv6Addr = DST_IP_V6;
        fn ip_builder(proto: IpProto) -> Ipv6PacketBuilder {
            let mut ip = Ipv6PacketBuilder::new(Self::SRC_IP, Self::DST_IP, 64, proto.into());
            ip.flowlabel(TEST_FLOWLABEL);
            ip
        }
    }

    fn ethernet_builder<I: TestIpExt>() -> EthernetFrameBuilder {
        EthernetFrameBuilder::new(SRC_MAC, DST_MAC, I::ETHER_TYPE, 0)
    }

    fn tcp_builder<I: TestIpExt>() -> TcpSegmentBuilder<I::Addr> {
        TcpSegmentBuilder::new(
            I::SRC_IP,
            I::DST_IP,
            TEST_SRC_PORT,
            TEST_DST_PORT,
            100,
            Some(200),
            1024,
        )
    }

    fn build_ethernet_tcp_packet<I: TestIpExt>() -> Vec<u8> {
        Buf::new(TEST_PAYLOAD.to_vec(), ..)
            .wrap_in(tcp_builder::<I>())
            .wrap_in(I::ip_builder(IpProto::Tcp))
            .wrap_in(ethernet_builder::<I>())
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .into_inner()
            .as_ref()
            .to_vec()
    }

    fn build_pure_ip_tcp_packet<I: TestIpExt>() -> Vec<u8> {
        Buf::new(TEST_PAYLOAD.to_vec(), ..)
            .wrap_in(tcp_builder::<I>())
            .wrap_in(I::ip_builder(IpProto::Tcp))
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .into_inner()
            .as_ref()
            .to_vec()
    }

    #[test_case(
        build_ethernet_tcp_packet::<Ipv4>(),
        GroFrameType::Ethernet,
        HeaderOffsets { ip_offset: 14, transport_offset: 34 },
        GroFlowId {
            link_layer: LinkLayerFlowId::Ethernet(EthernetFlowId {
                src_mac: SRC_MAC,
                dst_mac: DST_MAC,
                tag: None,
            }),
            ip: IpFlowId::Ipv4(Ipv4FlowId { src_ip: Ipv4::SRC_IP, dst_ip: Ipv4::DST_IP }),
            transport: TransportFlowId::Tcp(TcpFlowId {
                src_port: TEST_SRC_PORT,
                dst_port: TEST_DST_PORT,
            }),
        };
        "ethernet_ipv4_tcp"
    )]
    #[test_case(
        build_ethernet_tcp_packet::<Ipv6>(),
        GroFrameType::Ethernet,
        HeaderOffsets { ip_offset: 14, transport_offset: 54 },
        GroFlowId {
            link_layer: LinkLayerFlowId::Ethernet(EthernetFlowId {
                src_mac: SRC_MAC,
                dst_mac: DST_MAC,
                tag: None,
            }),
            ip: IpFlowId::Ipv6(Ipv6FlowId {
                src_ip: Ipv6::SRC_IP,
                dst_ip: Ipv6::DST_IP,
                flowlabel: TEST_FLOWLABEL,
            }),
            transport: TransportFlowId::Tcp(TcpFlowId {
                src_port: TEST_SRC_PORT,
                dst_port: TEST_DST_PORT,
            }),
        };
        "ethernet_ipv6_tcp"
    )]
    #[test_case(
        build_pure_ip_tcp_packet::<Ipv4>(),
        GroFrameType::PureIp(IpVersion::V4),
        HeaderOffsets { ip_offset: 0, transport_offset: 20 },
        GroFlowId {
            link_layer: LinkLayerFlowId::PureIp,
            ip: IpFlowId::Ipv4(Ipv4FlowId { src_ip: Ipv4::SRC_IP, dst_ip: Ipv4::DST_IP }),
            transport: TransportFlowId::Tcp(TcpFlowId {
                src_port: TEST_SRC_PORT,
                dst_port: TEST_DST_PORT,
            }),
        };
        "pure_ip_v4_tcp"
    )]
    #[test_case(
        build_pure_ip_tcp_packet::<Ipv6>(),
        GroFrameType::PureIp(IpVersion::V6),
        HeaderOffsets { ip_offset: 0, transport_offset: 40 },
        GroFlowId {
            link_layer: LinkLayerFlowId::PureIp,
            ip: IpFlowId::Ipv6(Ipv6FlowId {
                src_ip: Ipv6::SRC_IP,
                dst_ip: Ipv6::DST_IP,
                flowlabel: TEST_FLOWLABEL,
            }),
            transport: TransportFlowId::Tcp(TcpFlowId {
                src_port: TEST_SRC_PORT,
                dst_port: TEST_DST_PORT,
            }),
        };
        "pure_ip_v6_tcp"
    )]
    fn gro_packet_parse_success(
        packet_bytes: Vec<u8>,
        target: GroFrameType,
        expected_offsets: HeaderOffsets,
        expected_flow_id: GroFlowId,
    ) {
        let mut context = NetworkParsingContext::default();
        let parsed = GroPacket::parse(&packet_bytes, &target, &mut context)
            .expect("GroPacket::parse should succeed");
        assert_eq!(parsed.offsets, expected_offsets);
        assert_eq!(parsed.flow_id, expected_flow_id);
    }

    #[test]
    fn gro_ineligible_non_ip_ethertype() {
        let arp =
            ArpPacketBuilder::new(ArpOp::Request, SRC_MAC, Ipv4::SRC_IP, DST_MAC, Ipv4::DST_IP);
        let arp_bytes = arp
            .into_serializer()
            .wrap_in(EthernetFrameBuilder::new(SRC_MAC, DST_MAC, EtherType::Arp, 0))
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .unwrap_b()
            .as_ref()
            .to_vec();
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        assert!(GroPacket::parse(&arp_bytes, &GroFrameType::Ethernet, &mut context).is_none());
    }

    #[test]
    fn gro_ineligible_ipv4_options() {
        let ip = Ipv4PacketBuilderWithOptions::new(
            Ipv4::ip_builder(IpProto::Tcp),
            [Ipv4Option::RouterAlert { data: 0 }],
        )
        .unwrap();
        let packet = Buf::new(TEST_PAYLOAD.to_vec(), ..)
            .wrap_in(tcp_builder::<Ipv4>())
            .wrap_in(ip)
            .wrap_in(ethernet_builder::<Ipv4>())
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .into_inner()
            .as_ref()
            .to_vec();
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        assert!(GroPacket::parse(&packet, &GroFrameType::Ethernet, &mut context).is_none());
    }

    #[test]
    fn gro_ineligible_ipv4_mf_flag() {
        let mut ip = Ipv4::ip_builder(IpProto::Tcp);
        ip.mf_flag(true);
        let packet = Buf::new(TEST_PAYLOAD.to_vec(), ..)
            .wrap_in(tcp_builder::<Ipv4>())
            .wrap_in(ip)
            .wrap_in(ethernet_builder::<Ipv4>())
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .into_inner()
            .as_ref()
            .to_vec();
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        assert!(GroPacket::parse(&packet, &GroFrameType::Ethernet, &mut context).is_none());
    }

    #[test]
    fn gro_ineligible_ipv4_fragment_offset() {
        let mut ip = Ipv4::ip_builder(IpProto::Tcp);
        ip.fragment_offset(FragmentOffset::new(1).unwrap());
        let packet = Buf::new(TEST_PAYLOAD.to_vec(), ..)
            .wrap_in(tcp_builder::<Ipv4>())
            .wrap_in(ip)
            .wrap_in(ethernet_builder::<Ipv4>())
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .into_inner()
            .as_ref()
            .to_vec();
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        assert!(GroPacket::parse(&packet, &GroFrameType::Ethernet, &mut context).is_none());
    }

    #[test]
    fn gro_ineligible_ipv6_extension_headers() {
        let hbh_opt = [HopByHopOption {
            action: ExtensionHeaderOptionAction::SkipAndContinue,
            mutable: false,
            data: HopByHopOptionData::RouterAlert { data: 0 },
        }];
        let ip =
            Ipv6PacketBuilderWithHbhOptions::new(Ipv6::ip_builder(IpProto::Tcp), &hbh_opt).unwrap();
        let packet = Buf::new(TEST_PAYLOAD.to_vec(), ..)
            .wrap_in(tcp_builder::<Ipv6>())
            .wrap_in(ip)
            .wrap_in(ethernet_builder::<Ipv6>())
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .into_inner()
            .as_ref()
            .to_vec();
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        assert!(GroPacket::parse(&packet, &GroFrameType::Ethernet, &mut context).is_none());
    }

    #[test]
    fn gro_ineligible_non_tcp_transport_proto() {
        let udp =
            UdpPacketBuilder::new(Ipv4::SRC_IP, Ipv4::DST_IP, Some(TEST_SRC_PORT), TEST_DST_PORT);
        let packet = Buf::new(TEST_PAYLOAD.to_vec(), ..)
            .wrap_in(udp)
            .wrap_in(Ipv4::ip_builder(IpProto::Udp))
            .wrap_in(ethernet_builder::<Ipv4>())
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .into_inner()
            .as_ref()
            .to_vec();
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        assert!(GroPacket::parse(&packet, &GroFrameType::Ethernet, &mut context).is_none());
    }

    #[test]
    fn gro_corrupt_tcp_checksum() {
        let mut packet = build_ethernet_tcp_packet::<Ipv4>();
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        let parsed = GroPacket::parse(&packet, &GroFrameType::Ethernet, &mut context)
            .expect("should parse valid packet");
        let checksum_offset =
            parsed.offsets.transport_offset + packet_formats::tcp::CHECKSUM_OFFSET;
        packet[checksum_offset] ^= 0xff;

        // Fails when checksum verification is not offloaded.
        let mut context = NetworkParsingContext::default();
        assert!(GroPacket::parse(&packet, &GroFrameType::Ethernet, &mut context).is_none());

        // Succeeds when checksum verification is offloaded.
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        assert!(GroPacket::parse(&packet, &GroFrameType::Ethernet, &mut context).is_some());
    }

    #[test]
    fn gro_ineligible_pure_ip_version_mismatch() {
        let pure_v4 = build_pure_ip_tcp_packet::<Ipv4>();
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        assert!(
            GroPacket::parse(&pure_v4, &GroFrameType::PureIp(IpVersion::V6), &mut context)
                .is_none()
        );

        let pure_v6 = build_pure_ip_tcp_packet::<Ipv6>();
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        assert!(
            GroPacket::parse(&pure_v6, &GroFrameType::PureIp(IpVersion::V4), &mut context)
                .is_none()
        );
    }

    #[test]
    fn upgrades_csum_offload_on_verified_tcp() {
        let packet = build_ethernet_tcp_packet::<Ipv4>();
        let items: Vec<GroInputItem<TrackedBuffer, GroFrameType>> = vec![GroInputItem {
            buffer: TrackedBuffer::new(packet, true),
            target: GroFrameType::Ethernet,
            checksum_offload: ChecksumRxOffloading::default(),
        }];

        let mut storage = GroBufferStorage::new();
        let mut gro = GroIter::new(items.into_iter(), &mut storage, true);
        let item = gro.next().unwrap();
        assert_eq!(
            item.checksum_offload,
            ChecksumRxOffloading::Offloaded(Some(NonZeroU16::new(1).unwrap()))
        );
    }

    #[test]
    fn preserves_input_csum_offload_on_unparsed_tcp() {
        // Build a packet with IPv4 options: ineligible for GRO, so transport
        // checksum verification is never reached.
        let ip = Ipv4PacketBuilderWithOptions::new(
            Ipv4::ip_builder(IpProto::Tcp),
            [Ipv4Option::RouterAlert { data: 0 }],
        )
        .unwrap();
        let packet = Buf::new(TEST_PAYLOAD.to_vec(), ..)
            .wrap_in(tcp_builder::<Ipv4>())
            .wrap_in(ip)
            .wrap_in(ethernet_builder::<Ipv4>())
            .serialize_vec_outer(&mut NetworkSerializationContext::default())
            .unwrap()
            .into_inner()
            .as_ref()
            .to_vec();

        let items: Vec<GroInputItem<TrackedBuffer, GroFrameType>> = vec![GroInputItem {
            buffer: TrackedBuffer::new(packet, true),
            target: GroFrameType::Ethernet,
            checksum_offload: ChecksumRxOffloading::default(),
        }];

        let mut storage = GroBufferStorage::new();
        let mut gro = GroIter::new(items.into_iter(), &mut storage, true);
        let item = gro.next().unwrap();
        assert_eq!(item.checksum_offload, ChecksumRxOffloading::default());
    }

    #[test]
    fn preserves_input_csum_offload_on_corrupt_csum() {
        let mut packet = build_ethernet_tcp_packet::<Ipv4>();
        let mut context = NetworkParsingContext::new(ChecksumRxOffloading::FullyOffloaded);
        let parsed = GroPacket::parse(&packet, &GroFrameType::Ethernet, &mut context)
            .expect("should parse valid packet");
        let checksum_offset =
            parsed.offsets.transport_offset + packet_formats::tcp::CHECKSUM_OFFSET;
        packet[checksum_offset] ^= 0xff;

        let items: Vec<GroInputItem<TrackedBuffer, GroFrameType>> = vec![GroInputItem {
            buffer: TrackedBuffer::new(packet, true),
            target: GroFrameType::Ethernet,
            checksum_offload: ChecksumRxOffloading::default(),
        }];

        let mut storage = GroBufferStorage::new();
        let mut gro = GroIter::new(items.into_iter(), &mut storage, true);
        let item = gro.next().unwrap();
        assert_eq!(item.checksum_offload, ChecksumRxOffloading::default());
    }
}
