//! One ICMP echo, asked over the platform's own unprivileged datagram socket.
//!
//! **A host that cannot ask and an address that did not answer are different
//! answers**, and the caller gets them as `Err` and `Ok(false)`: a socket the
//! host refuses would otherwise red a boot for this machine's configuration.
//! A send the host's own routing refuses is the *second* of those and not the
//! first — [`wire_is_down`] is where that line is drawn.

use std::io;
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Type, code, checksum, identifier, sequence (RFC 792).
const HEADER: usize = 8;
const ECHO_REQUEST: u8 = 8;
const ECHO_REPLY: u8 = 0;

/// The payload that says a reply is this probe's.
const TOKEN: usize = 16;

/// Enough for any echo reply, its headers included.
const MOST: usize = 1024;

/// One per probe this process makes, so a reply to the probe before this one is
/// another probe's and not an answer here.
static ASKED: AtomicU64 = AtomicU64::new(0);

/// Whether `addr` answered an echo request inside `wait`.
///
/// `Err` is this host's failing and never the address's.
pub fn echo(addr: Ipv4Addr, wait: Duration) -> Result<bool, String> {
    let socket = open()?;
    let token = token();
    let request = request(&token);
    let deadline = Instant::now() + wait;
    if let Err(e) = socket.send_to(&request, (addr, 0)) {
        // The address this probe watches is down for the whole span it is asked
        // across, so the host's own routing has nowhere to put the request: that
        // is a second nothing answered, which is what `Ok(false)` already says.
        if wire_is_down(&e) {
            return Ok(false);
        }
        return Err(format!("this host could not send an ICMP echo request to {addr}: {e}"));
    }
    let mut buf = [0u8; MOST];
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(false);
        };
        // A window of zero is no wait at all to `SO_RCVTIMEO`, which is what a
        // caller of `set_read_timeout` may not pass either.
        if left.is_zero() {
            return Ok(false);
        }
        socket
            .set_read_timeout(Some(left))
            .map_err(|e| format!("this host would not bound an ICMP read by {left:?}: {e}"))?;
        match socket.recv_from(&mut buf) {
            Ok((got, from)) if is_reply(from.ip(), addr, &buf[..got], &token) => return Ok(true),
            // Somebody else's echo on the same socket: keep listening for this
            // probe's own until the window is spent.
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) =>
            {
                return Ok(false)
            }
            Err(e) => return Err(format!("this host could not read an ICMP reply: {e}")),
        }
    }
}

/// Whether a send failed because the wire is down right now, rather than
/// because this host cannot ask the question at all.
///
/// **Three errnos and no fourth, each named.** A send with nowhere to go is
/// refused by the kernel by name — `EHOSTUNREACH` where the address has no
/// neighbour to hand the frame to, `ENETUNREACH` where no route covers it,
/// `ENETDOWN` where the interface itself is gone — and across the span this
/// probe is asked over, every one of them is the machine being down, which is
/// the silence [`echo`] answers `Ok(false)` for. Everything else stays this
/// host's failing: a permission the kernel withheld, a socket that is closed,
/// a message it would not take.
fn wire_is_down(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::EHOSTUNREACH | libc::ENETUNREACH | libc::ENETDOWN))
}

/// The socket, or why this host would not open one.
fn open() -> Result<UdpSocket, String> {
    // SAFETY: `socket` answers a fresh descriptor or -1, and the descriptor it
    // answers is an `AF_INET` datagram socket — which is what `UdpSocket`
    // sends, receives and closes on.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP) };
    if fd < 0 {
        let why = io::Error::last_os_error();
        return Err(format!(
            "this host would not open an unprivileged ICMP datagram socket: {why}. On Linux \
             that socket is `net.ipv4.ping_group_range`'s to permit"
        ));
    }
    // SAFETY: `fd` is the descriptor just answered and nothing else owns it, so
    // the socket this makes is the only thing that will close it.
    Ok(unsafe { UdpSocket::from_raw_fd(fd) })
}

/// A payload no other probe of this host writes: the process, and which probe
/// of it this is.
fn token() -> [u8; TOKEN] {
    let mut token = [0u8; TOKEN];
    token[..8].copy_from_slice(&u64::from(std::process::id()).to_be_bytes());
    token[8..].copy_from_slice(&ASKED.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    token
}

/// One echo request carrying `token`.
///
/// **The checksum is this code's and not the kernel's**: one of the two hosts
/// computes it for a datagram ICMP socket and the other sends what it was given.
fn request(token: &[u8; TOKEN]) -> [u8; HEADER + TOKEN] {
    let mut message = [0u8; HEADER + TOKEN];
    message[0] = ECHO_REQUEST;
    message[HEADER..].copy_from_slice(token);
    let sum = checksum(&message).to_be_bytes();
    message[2..4].copy_from_slice(&sum);
    message
}

/// Whether `message`, which came `from`, is `addr`'s reply to the request
/// carrying `token`.
///
/// **Whose reply it was is half the question**: the socket is handed every ICMP
/// datagram this host receives, so another host returning this probe's payload
/// is not `addr` answering.
///
/// **Found by the payload rather than at an offset**: one host hands the
/// datagram over with the IPv4 header in front of the ICMP message and the
/// other without it, and the type byte is [`HEADER`] bytes before the payload
/// either way. The identifier is not read at all — Linux's ping socket
/// overwrites it with the socket's own port.
fn is_reply(from: IpAddr, addr: Ipv4Addr, message: &[u8], token: &[u8; TOKEN]) -> bool {
    from == addr
        && message
            .windows(TOKEN)
            .position(|window| window == token)
            .is_some_and(|at| at >= HEADER && message[at - HEADER] == ECHO_REPLY)
}

/// The internet checksum (RFC 1071): the one's-complement sum of the message as
/// 16-bit words, complemented.
fn checksum(message: &[u8]) -> u16 {
    let mut sum = 0u32;
    let (words, rest) = message.as_chunks::<2>();
    for word in words {
        sum += u32::from(u16::from_be_bytes(*word));
    }
    if let [odd] = rest {
        sum += u32::from(u16::from_be_bytes([*odd, 0]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An IPv4 header of no options, as one of the two hosts leaves in front of
    /// the message.
    const IPV4: [u8; 20] = [0x45, 0, 0, 36, 0, 0, 0, 0, 64, 1, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2];

    const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 1);

    fn reply_to(request: &[u8]) -> Vec<u8> {
        let mut reply = request.to_vec();
        reply[0] = ECHO_REPLY;
        reply[2..4].copy_from_slice(&[0, 0]);
        let sum = checksum(&reply).to_be_bytes();
        reply[2..4].copy_from_slice(&sum);
        reply
    }

    #[test]
    fn a_message_carrying_its_own_checksum_sums_to_zero() {
        let message = request(&token());
        assert_eq!(checksum(&message), 0, "{message:02x?}");
        // The worked example in RFC 1071 §3: the sum of those eight bytes is
        // `0xddf2`, and its complement is what the field carries.
        assert_eq!(checksum(&[0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7]), 0x220d);
        // An odd tail is the last byte padded, not dropped.
        assert_ne!(checksum(&[0x01, 0x02, 0x03]), checksum(&[0x01, 0x02]));
    }

    #[test]
    fn a_request_is_an_echo_request_carrying_its_token() {
        let token = token();
        let message = request(&token);
        assert_eq!(message[0], ECHO_REQUEST);
        assert_eq!(message[1], 0, "an echo carries code 0");
        assert_eq!(&message[HEADER..], &token[..]);
        // Two probes of one process are two tokens, so a late reply to the
        // first is not an answer to the second.
        assert_ne!(token, super::token());
    }

    #[test]
    fn the_checksum_is_the_field_rfc_792_gives_it() {
        let token = token();
        let message = request(&token);
        // The same request with the checksum field unwritten, built here rather
        // than taken from `request`, so where the sum went is what is compared.
        let mut unsummed = [0u8; HEADER + TOKEN];
        unsummed[0] = ECHO_REQUEST;
        unsummed[HEADER..].copy_from_slice(&token);
        assert_eq!(u16::from_be_bytes([message[2], message[3]]), checksum(&unsummed));
        assert_eq!(message[4..HEADER], unsummed[4..HEADER], "{message:02x?}");
    }

    #[test]
    fn a_reply_is_this_probes_however_the_host_hands_it_over() {
        let token = token();
        let reply = reply_to(&request(&token));
        assert!(is_reply(HOST.into(), HOST, &reply, &token));
        let headed: Vec<u8> = IPV4.iter().chain(reply.iter()).copied().collect();
        assert!(is_reply(HOST.into(), HOST, &headed, &token));
    }

    /// **A wire that is down right now is a second nothing answered, and
    /// nothing else is.**
    ///
    /// The case, off the bench: the loop flashed the stick, set `BootNext` and
    /// rebooted the machine, the address's neighbour entry lapsed while it was
    /// down, and the send earned `No route to host (os error 65)` — which the
    /// loop read as a host that could not ask and ended the run at exit 2, with
    /// the boot itself complete on the stick. The host was on the LAN either
    /// side of it. The three the kernel refuses a routeless send with are this
    /// probe's subject; the errnos below it are not, and a run that widened the
    /// first set into the second would spend its whole window calling a host
    /// with no socket a machine that never answered.
    #[test]
    fn a_wire_that_is_down_right_now_is_not_a_host_that_cannot_ask() {
        for errno in [libc::EHOSTUNREACH, libc::ENETUNREACH, libc::ENETDOWN] {
            assert!(wire_is_down(&io::Error::from_raw_os_error(errno)), "{errno}");
        }
        // The recorded failure's own number, as this host spells it.
        #[cfg(target_os = "macos")]
        assert_eq!(libc::EHOSTUNREACH, 65);
        for errno in [
            libc::EPERM,
            libc::EACCES,
            libc::EBADF,
            libc::ENOTSOCK,
            libc::EAFNOSUPPORT,
            libc::EMSGSIZE,
            libc::EINVAL,
        ] {
            assert!(!wire_is_down(&io::Error::from_raw_os_error(errno)), "{errno}");
        }
        // An error with no errno behind it is none of the three either.
        assert!(!wire_is_down(&io::Error::other("no errno")));
    }

    /// Everything that carries the bytes and is not this probe's answer.
    #[test]
    fn nothing_but_the_reply_to_this_probe_counts() {
        let token = token();
        let request = request(&token);
        // The request itself, which a host that loops its own traffic back
        // would otherwise read as an answer.
        assert!(!is_reply(HOST.into(), HOST, &request, &token));
        assert!(!is_reply(Ipv4Addr::new(10, 0, 0, 9).into(), HOST, &reply_to(&request), &token));
        // Another probe's reply, and a reply to nobody.
        assert!(!is_reply(HOST.into(), HOST, &reply_to(&super::request(&super::token())), &token));
        assert!(!is_reply(HOST.into(), HOST, &[], &token));
        // The token with nothing in front of it: a message that cannot carry a
        // type byte is not one.
        assert!(!is_reply(HOST.into(), HOST, &token, &token));
        // An unreachable message quoting this probe's request in its own body
        // is the address refusing, not answering: the type byte before the
        // payload is the quoted request's `8`.
        let mut unreachable = vec![3u8, 0, 0, 0, 0, 0, 0, 0];
        unreachable.extend_from_slice(&IPV4);
        unreachable.extend_from_slice(&request);
        assert!(!is_reply(HOST.into(), HOST, &unreachable, &token));
    }
}
