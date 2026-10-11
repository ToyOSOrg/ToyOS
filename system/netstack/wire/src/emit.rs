//! A builder's refusals, and its writes into a caller's buffer.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildError {
    BufferTooSmall,
    EthBodyTooLong,
    IpOptionsTooLong,
    IpTooLong,
    IpTtlZero,
    IpInvalidSource,
    UdpTooLong,
    TcpTooManySackBlocks,
    TcpWindowScaleTooLarge,
    TcpTooLong,
    IgmpReportAllHosts,
}

impl BuildError {
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

pub(crate) fn put<const N: usize>(out: &mut [u8], bytes: [u8; N]) -> Result<&mut [u8], BuildError> {
    let (head, rest) = out.split_first_chunk_mut::<N>().ok_or(BuildError::BufferTooSmall)?;
    *head = bytes;
    Ok(rest)
}

pub(crate) fn put_slice<'b>(out: &'b mut [u8], bytes: &[u8]) -> Result<&'b mut [u8], BuildError> {
    let (head, rest) = out.split_at_mut_checked(bytes.len()).ok_or(BuildError::BufferTooSmall)?;
    head.copy_from_slice(bytes);
    Ok(rest)
}

pub(crate) fn exact(out: &mut [u8], len: usize) -> Result<&mut [u8], BuildError> {
    out.split_at_mut_checked(len).map(|(head, _)| head).ok_or(BuildError::BufferTooSmall)
}

pub(crate) const fn be16x2(a: u16, b: u16) -> [u8; 4] {
    let [a0, a1] = a.to_be_bytes();
    let [b0, b1] = b.to_be_bytes();
    [a0, a1, b0, b1]
}
