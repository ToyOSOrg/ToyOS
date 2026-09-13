//! What a boot's log and its wire say about the address this machine took from
//! its network.
//!
//! **Text and frames in, verdicts out.** Nothing here touches a machine: the
//! QEMU arm and the T14 arm in `tests/common/lan.rs` read their answers through
//! this, so a guest and a laptop cannot be judged by different grammars.
//!
//! The `netd:` records below are the QEMU arm's; the T14 arm reads the kernel's
//! own records and `toyos_lanstate`'s exit code.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use crate::bootlog;

/// The PCI function the T14's card is, as `/sys/bus/pci/devices` spells it:
/// the cable the metal loop reaches a boot over while it runs.
pub const NIC: &str = "0000:00:1f.6";

/// The card on that function, as the kernel's records spell it.
const ID: &str = "8086:15fc";

/// The same function as the kernel's records spell it: `/sys` names the PCI
/// segment first and the kernel's records name the bus.
pub fn kernel_function(sysfs: &str) -> &str {
    sysfs.split_once(':').map_or(sysfs, |(_, function)| function)
}

/// The records both arms are written against, spelled once.
pub const MAC: &str = "netd: MAC ";
pub const LEASE: &str = "netd: DHCP: lease ";
pub const READY: &str = "netd: ready, at most ";
pub const NO_LEASE: &str = "netd: DHCP: no lease as ";

/// What netd writes where a server sent no router option, held to netd's own
/// source by [`tests::netd_writes_the_records_this_module_reads`].
const NO_GATEWAY: &str = "none";

/// The name this machine asks its network to record for it, held to netd's own
/// `dhcp::HOSTNAME` by [`tests::netd_writes_the_records_this_module_reads`].
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
    /// `None` where the server sent no router option: netd writes `gateway none`.
    pub gateway: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
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
    let gateway = match after(", gateway ", ",")?.as_str() {
        NO_GATEWAY => None,
        got => Some(address("the gateway's address", got.to_string())?),
    };
    let mut dns = Vec::new();
    for server in after(", dns [", "]")?.split_whitespace() {
        dns.push(address("a resolver", server.to_string())?);
    }
    Ok(Lease {
        address: address("this machine's address", host.to_string())?,
        prefix: prefix.parse().map_err(|_| unreadable("a prefix length"))?,
        server: address("the server's address", after(" from ", ",")?)?,
        gateway,
        dns,
    })
}

/// The record the kernel wrote as it gave this boot's NIC to a driver, or why
/// there is none.
///
/// **The needle is built out of [`NIC`]**, so the card the host read its MAC off
/// and the card the kernel gave away are one record rather than two checks that
/// could drift apart. A refusal is quoted only where it is this function's:
/// another function's is one fact with the wrong diagnosis attached to it.
pub fn handed_over(text: &str) -> Result<&str, String> {
    let function = kernel_function(NIC);
    let handed = format!("PCI {function} [{ID}] {}", bootlog::HANDED_OVER);
    if let Some(line) = text.lines().find(|l| l.contains(&handed)) {
        return Ok(line.trim());
    }
    let refused = format!("PCI {function} {}", bootlog::NOT_HANDED_OVER);
    match text.lines().find(|l| l.contains(&refused)) {
        Some(line) => Err(format!("the kernel refused {NIC}: {}", line.trim())),
        None => Err(format!(
            "no `{handed}` record and no refusal either: nothing on this machine claimed {ID}, \
             so `tests/lancase` was flashed onto a machine that has no such card"
        )),
    }
}

/// What the card raised into the process driving it, out of each CPU's census
/// of the source a user-driven device's vector is counted under.
///
/// **A lease with no interrupt is a contradiction**: DHCP is a round trip on the
/// wire and this kernel delivers a user-driven NIC's vector to the process that
/// claimed it, so a boot that leased and counted none did not lease — it read
/// somebody else's answer, or the census is not of this card. The counters are
/// cumulative, so a CPU's last census is its whole boot; a boot that printed
/// none at all is a different finding and says so rather than summing to zero.
pub fn interrupts_into_the_driver(census: &[(u32, u64)]) -> Result<u64, String> {
    if census.is_empty() {
        return Err("no `irq: cpu` census in this boot's log, so nothing here says whether the \
                    card raised an interrupt"
            .to_string());
    }
    let mut last: BTreeMap<u32, u64> = BTreeMap::new();
    for (cpu, raised) in census {
        last.insert(*cpu, *raised);
    }
    match last.values().sum::<u64>() {
        0 => Err("every `irq:` census of this boot counts none for the card netd drives: it \
                  raised no interrupt, so nothing it received reached the driver"
            .to_string()),
        raised => Ok(raised),
    }
}

/// What the job that asked netd left in its `exit:` record, judged against the
/// cable the metal loop reached this boot over.
///
/// `Ok(())` is netd holding the address that answered the host's ping, on the
/// card whose MAC that host read off the wire — the two halves of the cable,
/// agreed to from inside the machine. Everything else is one finding, and the
/// three kinds are separate on purpose: a machine that took no lease, a machine
/// that took another network's, and a job that never got to ask are different
/// defects and a T14 boot says nothing else about which.
pub fn job_said(code: i32, addr: Ipv4Addr, mac: &str) -> Result<(), String> {
    let bytes = mac_bytes(mac)
        .ok_or_else(|| format!("this readback's wire MAC reads {mac:?}, which is no MAC"))?;
    match toyos_lanstate::said(code) {
        toyos_lanstate::Said::Fingerprint(got) => {
            let want = toyos_lanstate::fingerprint(bytes, addr);
            if got == want {
                return Ok(());
            }
            Err(format!(
                "the job that asked netd exited {got} and {addr} on {mac} folds to {want}: the \
                 address netd held and the card it drove are not the pair this cable carried"
            ))
        }
        toyos_lanstate::Said::Refused(refusal) => {
            Err(format!("the job that asked netd exited {code}: {}", refusal.why()))
        }
        toyos_lanstate::Said::Foreign(code) => Err(format!(
            "the job that asked netd exited {code}, which is no word of its grammar: it died \
             before it could ask"
        )),
    }
}

/// `/sys/class/net/<i>/address`'s six bytes, as the metal loop passes them on.
fn mac_bytes(text: &str) -> Option<[u8; 6]> {
    let mut bytes = [0u8; 6];
    let mut fields = text.split(':');
    for byte in bytes.iter_mut() {
        *byte = u8::from_str_radix(fields.next()?, 16).ok()?;
    }
    fields.next().is_none().then_some(bytes)
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

    fn dewrapped(source: &str) -> String {
        source.split("\\\n").map(str::trim_start).collect()
    }

    /// **Held here and not by netd's source**: no netd literal wraps at a head
    /// today, so a scan over the file as written is green either way.
    #[test]
    fn a_head_rustfmt_split_across_two_lines_reads_as_one() {
        let wrapped = "    crate::say!(\"netd: DHCP: \\\n                 lease {}/{} from {}\");";
        let head = format!("\"{LEASE}");
        assert!(!wrapped.contains(&head), "this fixture carries no wrap to close up");
        assert!(dewrapped(wrapped).contains(&head), "the wrap still swallows {head:?}");
    }

    /// Nothing links the two crates, so every record this module and `on_metal`
    /// rest on is held to netd's own source: a reworded one reds by its name
    /// rather than as an absence.
    #[test]
    fn netd_writes_the_records_this_module_reads() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("userland/netd/src");
        let read = |name: &str| {
            let at = root.join(name);
            std::fs::read_to_string(&at).unwrap_or_else(|e| panic!("{}: {e}", at.display()))
        };
        let source = dewrapped(&["main.rs", "i219.rs", "dhcp.rs"].map(read).join("\n"));
        for head in [MAC, LEASE, READY, NO_LEASE] {
            assert!(source.contains(&format!("\"{head}")), "netd opens no record with {head:?}");
        }
        assert!(
            source.contains(&format!("\"{NO_GATEWAY}\"")),
            "netd writes no {NO_GATEWAY:?} where a server sent no router option"
        );
        assert!(
            crate::bootlog::declares(&source, &format!("b\"{HOSTNAME}\"")),
            "netd declares no constant equal to b\"{HOSTNAME}\""
        );
    }

    /// **A word two crates send down one connection may be one word only.**
    /// `toyos_lanstate::ASK` is netd's and is declared outside the SDK that
    /// owns every other, so nothing but this holds the two apart: a collision
    /// would make netd answer a `MsgType` with its own state and a client read
    /// that state as the answer it asked for.
    #[test]
    fn the_state_word_is_no_message_the_sdk_sends() {
        let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("toyos/src/net.rs");
        let source = std::fs::read_to_string(&at)
            .unwrap_or_else(|e| panic!("{}: {e}", at.display()));
        let words = sdk_words(&source);
        // The scan before the claim: a parser that found nothing would pass
        // this test on any word at all.
        for (name, word) in [("TcpClose", 4), ("TcpAcceptPiped", 22), ("Error", 129)] {
            assert!(words.contains(&word), "the scan missed `{name} = {word}`: {words:?}");
        }
        let ask = toyos_lanstate::ASK;
        for spelling in [
            format!("{ask}"),
            format!("{ask:#x}"),
            format!("{ask:#o}"),
            format!("{ask:#b}"),
            "4_997_454".to_string(),
            "0x4c_41_4e".to_string(),
            "4997454u32".to_string(),
            "0x4c414e_u32".to_string(),
        ] {
            assert_eq!(sdk_words(&format!("    Ask = {spelling},\n")), [ask], "{spelling}");
        }
        assert!(
            !words.contains(&toyos_lanstate::ASK),
            "netd's state word {} is also one of the SDK's: {words:?}",
            toyos_lanstate::ASK
        );
    }

    /// Every number `toyos::net` puts in an IPC header, out of the SDK's own
    /// source: the discriminants of its two `#[repr(u32)]` enums.
    fn sdk_words(source: &str) -> Vec<u32> {
        source
            .lines()
            .filter_map(|line| line.trim_end().strip_suffix(',')?.split_once(" = "))
            .filter_map(|(_, value)| discriminant(value.trim()))
            .collect()
    }

    /// One discriminant, in every notation Rust spells an integer literal in.
    fn discriminant(value: &str) -> Option<u32> {
        let value = value.replace('_', "");
        let (radix, rest) = match value.get(..2) {
            Some("0b") => (2, &value[2..]),
            Some("0o") => (8, &value[2..]),
            Some("0x") => (16, &value[2..]),
            _ => (10, &value[..]),
        };
        let end = rest.find(|c: char| !c.is_digit(radix)).unwrap_or(rest.len());
        let (digits, suffix) = rest.split_at(end);
        const SUFFIXES: [&str; 13] = [
            "", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128",
            "isize",
        ];
        SUFFIXES.contains(&suffix).then(|| u32::from_str_radix(digits, radix).ok())?
    }

    /// The whole grammar as `on_metal` reads it, against the pair a readback
    /// carries: the fold agreeing, the fold not agreeing, every refusal, and a
    /// code the job never wrote.
    #[test]
    fn one_exit_code_is_read_against_the_cable_the_loop_reached() {
        let addr = Ipv4Addr::new(192, 168, 1, 42);
        let mac = "54:bf:64:2f:0a:1c";
        let bytes = mac_bytes(mac).expect("a MAC");
        assert_eq!(job_said(toyos_lanstate::fingerprint(bytes, addr), addr, mac), Ok(()));
        // The same boot, the card that held the address before it swapped.
        let other = mac_bytes("54:bf:64:2f:0a:1d").expect("a MAC");
        let why = job_said(toyos_lanstate::fingerprint(other, addr), addr, mac)
            .expect_err("another card's fold is not this one's");
        assert!(why.contains("not the pair this cable carried"), "{why}");
        let why = job_said(toyos_lanstate::Refusal::NoLease.code(), addr, mac)
            .expect_err("a machine with no address");
        assert!(why.contains("no address"), "{why}");
        let why = job_said(0, addr, mac).expect_err("a job that exited before it asked");
        assert!(why.contains("no word of its grammar"), "{why}");
        // The MAC the loop read, refused where it is not one rather than folded
        // into a mismatch that names the card.
        let why = job_said(0, addr, "enp0s31f6").expect_err("an interface name is not a MAC");
        assert!(why.contains("which is no MAC"), "{why}");
        for not_a_mac in ["54:bf:64:2f:0a", "54:bf:64:2f:0a:1c:ff", "54:bf:64:2f:0a:zz", ""] {
            assert_eq!(mac_bytes(not_a_mac), None, "{not_a_mac:?}");
        }
    }

    /// The judge's hand-over needle is built out of [`NIC`], so the function the
    /// loop reached the boot over and the function the kernel handed to netd are
    /// one fact rather than two spellings that could drift apart.
    #[test]
    fn the_function_the_loop_reaches_is_the_one_the_kernel_records() {
        assert_eq!(kernel_function(NIC), "00:1f.6");
        // The segment and nothing else: a needle short of the bus would find
        // the hand-over record of whatever function shared its device number.
        assert_eq!(format!("0000:{}", kernel_function(NIC)), NIC);
    }

    /// The kernel's own record, as `kernel/src/pcidev/mod.rs` writes it.
    fn handover_line(function: &str) -> String {
        format!(
            "[2026-09-13 14:27:17 1.450 cpu0] pcidev: PCI {function} [{ID}] {} 0, vector 0x28 \
             on MSI",
            bootlog::HANDED_OVER
        )
    }

    #[test]
    fn the_hand_over_this_boot_owes_is_read_of_this_function_alone() {
        let line = handover_line(kernel_function(NIC));
        assert_eq!(handed_over(&format!("boot\n{line}\nmore\n")), Ok(line.as_str()));
        // Another function's hand-over is not this one's.
        let elsewhere = handover_line("00:1f.3");
        let why = handed_over(&elsewhere).expect_err("a different function");
        assert!(why.contains("has no such card"), "{why}");
        // This function's refusal is quoted as the diagnosis…
        let refused = format!(
            "[x] pcidev: PCI {} {} — it would have no address space of its own",
            kernel_function(NIC),
            bootlog::NOT_HANDED_OVER
        );
        let why = handed_over(&refused).expect_err("a function the kernel would not give away");
        assert!(why.contains("no address space of its own"), "{why}");
        // …and another function's is not, because quoting it would put the
        // wrong diagnosis on the one fact this boot has.
        let others = format!("[x] pcidev: PCI 00:1f.3 {} — x", bootlog::NOT_HANDED_OVER);
        let why = handed_over(&others).expect_err("another function's refusal");
        assert!(why.contains("has no such card"), "{why}");
        assert!(handed_over("").is_err());
    }

    #[test]
    fn a_lease_with_no_interrupt_and_a_boot_with_no_census_are_different_findings() {
        assert_eq!(interrupts_into_the_driver(&[(0, 1), (0, 7), (1, 2)]), Ok(9));
        let why = interrupts_into_the_driver(&[]).expect_err("a boot that printed no census");
        assert!(why.contains("nothing here says whether"), "{why}");
        let why = interrupts_into_the_driver(&[(0, 0), (1, 0)]).expect_err("a silent card");
        assert!(why.contains("raised no interrupt"), "{why}");
    }

    #[test]
    fn a_lease_record_is_read_field_by_field() {
        assert_eq!(
            lease_in(LEASED),
            Ok(Lease {
                address: Ipv4Addr::new(10, 0, 2, 15),
                prefix: 24,
                server: Ipv4Addr::new(10, 0, 2, 2),
                gateway: Some(Ipv4Addr::new(10, 0, 2, 2)),
                dns: vec![Ipv4Addr::new(10, 0, 2, 3), Ipv4Addr::new(10, 0, 2, 4)],
            })
        );
        // A lease with no resolvers at all is a lease, and an empty list is not
        // a missing field.
        let none = LEASED.replace("10.0.2.3 10.0.2.4", "");
        assert!(lease_in(&none).expect("a lease").dns.is_empty());
        // A server that sent no router option leases too, and netd says so.
        let routerless = LEASED.replace("gateway 10.0.2.2", "gateway none");
        assert_eq!(lease_in(&routerless).expect("a lease").gateway, None);
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

    /// **The server's own echo of the option is not the client asking.** A walk
    /// keyed on the destination port would count the echo below and report the
    /// question as asked when nothing asked it.
    #[test]
    fn only_the_direction_leaving_the_client_counts() {
        let option = host_name_option();
        assert_eq!(asked_under_its_own_name(&pcap(&[from_client(&option)])), Ok(()));
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
