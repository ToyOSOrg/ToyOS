//! What every builder shares: its refusals, and writing into a caller's buffer.

/// Why a builder refused to produce a packet. A built packet is never
/// truncated or clamped to make it fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    /// The caller's buffer is shorter than the packet.
    BufferTooSmall,
    /// An Ethernet body above 1,500 bytes.
    EthBodyTooLong,
    /// More than 40 bytes of IPv4 options once padded.
    IpOptionsTooLong,
    /// An IPv4 total length above 65,535.
    IpTooLong,
    /// A TTL of 0 (RFC 1122 §3.2.1.7).
    IpTtlZero,
    /// A multicast, broadcast or class E source (RFC 1122 §3.2.1.3).
    IpInvalidSource,
    /// A UDP length above 65,535.
    UdpTooLong,
    /// More SACK blocks than the 40-byte option area holds (RFC 2018 §3).
    TcpTooManySackBlocks,
    /// A Window Scale shift above 14 (RFC 7323 §2.3).
    TcpWindowScaleTooLarge,
    /// A TCP length above 65,535 less the IPv4 header.
    TcpTooLong,
    /// A report for 224.0.0.1, which is never reported (RFC 2236 §6).
    IgmpReportAllHosts,
}

impl BuildError {
    /// The counter this refusal increments.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BufferTooSmall => "build.buffer-too-small",
            Self::EthBodyTooLong => "eth.body-too-long",
            Self::IpOptionsTooLong => "ip.options-too-long",
            Self::IpTooLong => "ip.too-long",
            Self::IpTtlZero => "ip.ttl-zero",
            Self::IpInvalidSource => "ip.invalid-source",
            Self::UdpTooLong => "udp.too-long",
            Self::TcpTooManySackBlocks => "tcp.too-many-sack-blocks",
            Self::TcpWindowScaleTooLarge => "tcp.window-scale-too-large",
            Self::TcpTooLong => "tcp.too-long",
            Self::IgmpReportAllHosts => "igmp.report-all-hosts",
        }
    }
}

/// Writes `bytes` at the front of `out` and returns what follows them.
pub(crate) fn put<const N: usize>(out: &mut [u8], bytes: [u8; N]) -> Result<&mut [u8], BuildError> {
    let (head, rest) = out.split_first_chunk_mut::<N>().ok_or(BuildError::BufferTooSmall)?;
    *head = bytes;
    Ok(rest)
}

/// Copies `bytes` to the front of `out` and returns what follows them.
pub(crate) fn put_slice<'b>(out: &'b mut [u8], bytes: &[u8]) -> Result<&'b mut [u8], BuildError> {
    let (head, rest) = out.split_at_mut_checked(bytes.len()).ok_or(BuildError::BufferTooSmall)?;
    head.copy_from_slice(bytes);
    Ok(rest)
}

/// The first `len` bytes of `out`, which a packet of that length is written into.
pub(crate) fn exact(out: &mut [u8], len: usize) -> Result<&mut [u8], BuildError> {
    out.split_at_mut_checked(len).map(|(head, _)| head).ok_or(BuildError::BufferTooSmall)
}

/// Two big-endian 16-bit fields as four bytes.
pub(crate) const fn be16x2(a: u16, b: u16) -> [u8; 4] {
    let [a0, a1] = a.to_be_bytes();
    let [b0, b1] = b.to_be_bytes();
    [a0, a1, b0, b1]
}
