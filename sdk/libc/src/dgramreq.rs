//! What a datagram send asks netstack, in order. It reads nothing but what it
//! is handed, so the host tests it (`toyos-libc-copies`).

/// A datagram socket's calls of netstack.
pub(crate) trait Datagrams {
    type Error;
    /// Whether netstack holds the socket.
    fn held(&self) -> bool;
    /// Bind it at `ip` and `port`, port 0 asking the stack to choose one.
    fn bind(&mut self, ip: [u8; 4], port: u16) -> Result<(), Self::Error>;
    /// Send `data` to `ip` and `port`: the bytes it took.
    fn send(&mut self, data: &[u8], ip: [u8; 4], port: u16) -> Result<usize, Self::Error>;
}

/// `data` to `ip` and `port`. POSIX `sendto`: a socket not yet bound is bound
/// at its first send, at any address, to a port the stack chooses; a
/// connected socket's `send` is this, to its peer.
pub(crate) fn send_to<D: Datagrams>(socket: &mut D, data: &[u8], ip: [u8; 4], port: u16) -> Result<usize, D::Error> {
    if !socket.held() {
        socket.bind([0; 4], 0)?;
    }
    socket.send(data, ip, port)
}
