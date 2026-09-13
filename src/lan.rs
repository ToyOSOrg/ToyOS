//! What a boot's log and its wire say about the address this machine took from
//! its network.
//!
//! **Text and frames in, verdicts out.** Nothing here touches a machine: the
//! QEMU arm and the T14 arm in `tests/common/lan.rs` read their answers through
//! this, so a guest and a laptop cannot be judged by different grammars.

#![forbid(unsafe_code)]

use std::net::Ipv4Addr;

/// The records both arms are written against, spelled once.
pub const MAC: &str = "netd: MAC ";
pub const LEASE: &str = "netd: DHCP: lease ";
pub const LINK_UP: &str = "netd: I219: link up at ";
pub const READY: &str = "netd: ready, at most ";
pub const NO_LEASE: &str = "netd: DHCP: no lease as ";

/// The name this machine asks its network to record for it, held to netd's own
/// `dhcp::HOSTNAME` by [`tests::netd_declares_the_name_this_module_spells`].
pub const HOSTNAME: &str = "toyos-t14";

/// RFC 2132 §3.14: the kind, the length, and the name.
fn host_name_option() -> Vec<u8> {
    let mut option = vec![12, HOSTNAME.len() as u8];
    option.extend_from_slice(HOSTNAME.as_bytes());
    option
}

/// One lease, as the record carries it.
#[derive(Debug, PartialEq, Eq)]
pub struct Lease {
    pub address: Ipv4Addr,
    pub prefix: u8,
    pub server: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub dns: Vec<Ipv4Addr>,
    /// Milliseconds between netd starting and the lease landing.
    pub ms: u64,
}

/// The lease record, read out of a boot's log.
///
/// Anchored on the record's own words rather than on positions, so a line that
/// grows a field still reads and one that loses a field is refused by name.
pub fn lease_in(text: &str) -> Result<Lease, String> {
    let line = text
        .lines()
        .find(|l| l.contains(LEASE))
        .ok_or_else(|| format!("no {LEASE:?} record: this boot took no address from its network"))?;
    let unreadable = |what: &str| format!("{line:?} carries no {what}");
    let after = |head: &str, tail: &str| -> Result<String, String> {
        let (_, rest) = line.split_once(head).ok_or_else(|| unreadable(head))?;
        let (got, _) = rest.split_once(tail).ok_or_else(|| unreadable(tail))?;
        Ok(got.to_string())
    };
    let address = |what: &'static str, got: String| -> Result<Ipv4Addr, String> {
        got.parse().map_err(|_| format!("{line:?} reads {got:?} where {what} belongs"))
    };
    let cidr = after(LEASE, " from ")?;
    let (host, prefix) = cidr.split_once('/').ok_or_else(|| unreadable("an address/prefix"))?;
    let mut dns = Vec::new();
    for server in after(", dns [", "]")?.split_whitespace() {
        dns.push(address("a resolver", server.to_string())?);
    }
    Ok(Lease {
        address: address("this machine's address", host.to_string())?,
        prefix: prefix.parse().map_err(|_| unreadable("a prefix length"))?,
        server: address("the server's address", after(" from ", ",")?)?,
        gateway: address("the gateway's address", after(", gateway ", ",")?)?,
        dns,
        ms: after("], ", " ms after netd came up")?
            .parse()
            .map_err(|_| unreadable("a millisecond count"))?,
    })
}

/// How long after the driver came up the link did, out of the driver's own
/// record.
pub fn link_up_ms(text: &str) -> Result<u64, String> {
    let line = text.lines().find(|l| l.contains(LINK_UP)).ok_or_else(|| {
        format!("no {LINK_UP:?} record: this boot's card never reported a link")
    })?;
    let (_, rest) = line.split_once(", ").ok_or_else(|| {
        format!("{line:?} says nothing about when the link came up, so the card was already up")
    })?;
    rest.split_once(" ms after the driver came up")
        .ok_or_else(|| format!("{line:?} carries no link-up time"))?
        .0
        .parse()
        .map_err(|_| format!("{line:?} carries no readable link-up time"))
}

/// **The one place the host-name option can be read.** A server that ignores it
/// answers the same lease either way, so the frames the client sent are the only
/// evidence that it asked at all — and `filter-dump` records both directions, so
/// a frame counts only where it is IPv4 over UDP *leaving* the client's own port.
pub fn asked_under_its_own_name(pcap: &[u8]) -> Result<(), String> {
    const LITTLE_ENDIAN_PCAP: [u8; 4] = [0xd4, 0xc3, 0xb2, 0xa1];
    const GLOBAL_HEADER: usize = 24;
    const RECORD_HEADER: usize = 16;
    /// Ethernet, an IPv4 header carrying no options, and UDP.
    const HEADERS: usize = 14 + 20 + 8;
    if pcap.get(..LITTLE_ENDIAN_PCAP.len()) != Some(&LITTLE_ENDIAN_PCAP[..]) {
        return Err("this file does not open with a little-endian pcap header".to_string());
    }
    let option = host_name_option();
    let (mut at, mut sent, mut asked) = (GLOBAL_HEADER, 0usize, false);
    while let Some(header) = pcap.get(at..at + RECORD_HEADER) {
        let len = u32::from_le_bytes(header[8..12].try_into().expect("four bytes")) as usize;
        let frame = pcap.get(at + RECORD_HEADER..at + RECORD_HEADER + len).ok_or_else(|| {
            format!("this pcap's record at byte {at} names {len} bytes the file has not")
        })?;
        at += RECORD_HEADER + len;
        if frame.len() > HEADERS
            && frame[12..14] == [0x08, 0x00]
            && frame[23] == 17
            && frame[34..36] == [0, 68]
        {
            sent += 1;
            asked |= frame.windows(option.len()).any(|w| w == option);
        }
    }
    if !asked {
        return Err(format!(
            "none of the {sent} frame(s) this client sent a DHCP server carries the host-name \
             option {option:?}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEASED: &str = "[2026-09-08 16:08:23 2.100 cpu0] netd: DHCP: lease 10.0.2.15/24 from \
                          10.0.2.2, gateway 10.0.2.2, dns [10.0.2.3 10.0.2.4], 412 ms after netd \
                          came up";

    /// Nothing links the two crates: netd is a `no_std`-shaped userland binary
    /// and this is the build system, so the name both ends spell is held to
    /// netd's own declaration by reading its source.
    #[test]
    fn netd_declares_the_name_this_module_spells() {
        let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("userland/netd/src/dhcp.rs");
        let source = std::fs::read_to_string(&at).expect("netd's dhcp module");
        assert!(
            crate::bootlog::declares(&source, &format!("b\"{HOSTNAME}\"")),
            "{} declares no constant equal to b\"{HOSTNAME}\"",
            at.display()
        );
    }

    /// **Every field the lease decided, typed**, so a record that grew a field
    /// still reads and one that lost a field is refused by the field's name.
    #[test]
    fn a_lease_record_is_read_field_by_field() {
        assert_eq!(
            lease_in(LEASED),
            Ok(Lease {
                address: Ipv4Addr::new(10, 0, 2, 15),
                prefix: 24,
                server: Ipv4Addr::new(10, 0, 2, 2),
                gateway: Ipv4Addr::new(10, 0, 2, 2),
                dns: vec![Ipv4Addr::new(10, 0, 2, 3), Ipv4Addr::new(10, 0, 2, 4)],
                ms: 412,
            })
        );
        // A lease with no resolvers at all is a lease, and an empty list is not
        // a missing field.
        let none = LEASED.replace("10.0.2.3 10.0.2.4", "");
        assert!(lease_in(&none).expect("a lease").dns.is_empty());
    }

    #[test]
    fn a_record_missing_a_field_is_refused_by_that_fields_name() {
        assert!(lease_in("nothing here\n").unwrap_err().contains("took no address"));
        for (cut, says) in [
            (", gateway 10.0.2.2", "gateway"),
            (", dns [10.0.2.3 10.0.2.4]", "dns ["),
            ("/24", "an address/prefix"),
        ] {
            let why = lease_in(&LEASED.replace(cut, "")).expect_err(cut);
            assert!(why.contains(says), "{cut}: {why}");
        }
        // A field that is there and is not what it claims to be.
        let why = lease_in(&LEASED.replace("gateway 10.0.2.2", "gateway enp0s31f6"))
            .expect_err("an interface name is not a gateway");
        assert!(why.contains("the gateway's address"), "{why}");
        let why = lease_in(&LEASED.replace("dns [10.0.2.3", "dns [fe80::1"))
            .expect_err("an IPv6 resolver is not one this record can carry");
        assert!(why.contains("a resolver"), "{why}");
        let why = lease_in(&LEASED.replace("412 ms", "later ms")).expect_err("no milliseconds");
        assert!(why.contains("a millisecond count"), "{why}");
    }

    #[test]
    fn a_link_that_was_already_up_is_told_from_one_that_came_up() {
        let came_up = format!("[x] {LINK_UP}1000 Mb/s, 2400 ms after the driver came up");
        assert_eq!(link_up_ms(&came_up), Ok(2_400));
        assert!(link_up_ms("nothing\n").unwrap_err().contains("never reported a link"));
        let why = link_up_ms(&format!("[x] {LINK_UP}1000 Mb/s")).expect_err("no comma");
        assert!(why.contains("already up"), "{why}");
    }

    /// One pcap record per frame, with the timestamps a reader here never looks
    /// at left zero.
    fn pcap(frames: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0xd4, 0xc3, 0xb2, 0xa1];
        out.extend_from_slice(&[0u8; 20]);
        for frame in frames {
            out.extend_from_slice(&[0u8; 8]);
            out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
            out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
            out.extend_from_slice(frame);
        }
        out
    }

    /// One frame: an ethertype, an IPv4 protocol, the two UDP ports, and a
    /// payload.
    fn frame(ethertype: [u8; 2], protocol: u8, src: u16, dst: u16, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; 14 + 20 + 8];
        frame[12..14].copy_from_slice(&ethertype);
        frame[23] = protocol;
        frame[34..36].copy_from_slice(&src.to_be_bytes());
        frame[36..38].copy_from_slice(&dst.to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    fn from_client(payload: &[u8]) -> Vec<u8> {
        frame([0x08, 0x00], 17, 68, 67, payload)
    }

    #[test]
    fn a_frame_carrying_the_option_out_of_the_clients_own_port_is_the_evidence() {
        let asked = pcap(&[from_client(&host_name_option())]);
        assert_eq!(asked_under_its_own_name(&asked), Ok(()));
    }

    /// **The server's own echo of the option is not the client asking.** A walk
    /// keyed on the destination port would count the frame below and report the
    /// question as asked when nothing asked it.
    #[test]
    fn only_the_direction_leaving_the_client_counts() {
        let option = host_name_option();
        let echoed = frame([0x08, 0x00], 17, 67, 68, &option);
        let why = asked_under_its_own_name(&pcap(std::slice::from_ref(&echoed)))
            .expect_err("a server's reply is not this client asking");
        assert!(why.contains("none of the 0 frame(s)"), "{why}");
        // The same echo beside a client frame that asked nothing.
        let why = asked_under_its_own_name(&pcap(&[echoed, from_client(&[53, 1, 1])]))
            .expect_err("the one frame the client sent carried no name");
        assert!(why.contains("none of the 1 frame(s)"), "{why}");
    }

    #[test]
    fn a_frame_that_is_not_ipv4_over_udp_carries_no_option_here() {
        let option = host_name_option();
        // ARP, and IPv4 carrying TCP: both hold the bytes and neither is a
        // DHCP request.
        for stray in [
            frame([0x08, 0x06], 17, 68, 67, &option),
            frame([0x08, 0x00], 6, 68, 67, &option),
        ] {
            let why = asked_under_its_own_name(&pcap(&[stray])).expect_err("not a DHCP frame");
            assert!(why.contains("none of the 0 frame(s)"), "{why}");
        }
        // A frame with the headers and no payload at all.
        let bare = from_client(&[]);
        assert!(asked_under_its_own_name(&pcap(&[bare])).is_err());
    }

    #[test]
    fn a_file_that_is_not_a_pcap_and_a_record_past_its_end_are_refused_by_name() {
        assert!(asked_under_its_own_name(b"").unwrap_err().contains("little-endian pcap"));
        assert!(
            asked_under_its_own_name(b"\xa1\xb2\xc3\xd4rest").unwrap_err().contains("pcap header")
        );
        let mut truncated = pcap(&[from_client(&host_name_option())]);
        truncated.truncate(truncated.len() - 4);
        let why = asked_under_its_own_name(&truncated).expect_err("the last record is cut short");
        assert!(why.contains("bytes the file has not"), "{why}");
    }
}
