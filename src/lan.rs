//! What a boot's log and its wire say about the address this machine took from
//! its network.
//!
//! **Text and frames in, verdicts out.** Nothing here touches a machine: the
//! QEMU arm and the T14 arm in `tests/common/lan.rs` read their answers through
//! this, so a guest and a laptop cannot be judged by different grammars.

#![forbid(unsafe_code)]

use std::net::Ipv4Addr;

use crate::bootlog::message;

/// The records both arms are written against, spelled once.
pub const MAC: &str = "netd: MAC ";
pub const LEASE: &str = "netd: DHCP: lease ";
pub const LINK_UP: &str = "netd: I219: link up at ";
pub const READY: &str = "netd: ready, at most ";
pub const NO_LEASE: &str = "netd: DHCP: no lease as ";

/// The name this machine asks its network to record for it, and answers for as
/// `<name>.local`; held to netd's own `dhcp::HOSTNAME` by
/// [`tests::netd_declares_the_name_this_module_spells`].
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

/// What netd wrote in a boot's `/log`: the text of each of its lines.
pub fn netd_records(text: &str) -> String {
    crate::bootlog::lines_of(text, "netd")
}

/// The T14's I219, as the kernel's hand-over record spells its id.
pub const I219: &str = "8086:15fc";

/// The kernel's two records that say a message the I219 raised reached a CPU.
#[derive(Debug, PartialEq, Eq)]
pub struct Delivery {
    pub slot: usize,
    pub vector: u8,
    pub handed: String,
    pub took: String,
}

/// Whether a message the I219 raised reached a CPU, out of the kernel's own
/// records: the hand-over that names the function's slot and vector, and that
/// slot's first-message record on that vector, after it and before the slot's
/// next hand-over.
///
/// **Whole messages in the kernel's spelling, never a substring**, so another
/// writer's line, another slot's record, and a record with no hand-over of this
/// function before it are each refused by what they lack.
pub fn delivered(text: &str) -> Result<Delivery, String> {
    let handed = format!("[{I219}] handed over on slot ");
    let Some((at, line)) = text.lines().enumerate().find(|(_, l)| {
        message(l).is_some_and(|m| m.starts_with("pcidev: PCI ") && m.contains(&handed))
    }) else {
        return Err(match text.lines().find(|l| l.contains("NOT HANDED OVER")) {
            Some(refused) => format!(
                "no `{handed}` record, and the kernel refused a function: {}",
                refused.trim()
            ),
            None => format!(
                "no `{handed}` record and no refusal either: nothing on this boot claimed {I219}"
            ),
        });
    };
    let (slot, vector) = message(line)
        .and_then(|m| m.split_once(&handed))
        .and_then(|(_, tail)| tail.split_once(", vector 0x"))
        .and_then(|(slot, vector)| {
            Some((slot.parse::<usize>().ok()?, u8::from_str_radix(vector, 16).ok()?))
        })
        .ok_or_else(|| format!("{line:?} carries no readable slot and vector"))?;

    // A hand-over clears the slot's record, so the next one of this slot ends
    // the span the record can answer this claim in.
    let again = format!("handed over on slot {slot},");
    let end = text
        .lines()
        .enumerate()
        .skip(at + 1)
        .find(|(_, l)| {
            message(l).is_some_and(|m| m.starts_with("pcidev: PCI ") && m.contains(&again))
        })
        .map(|(i, l)| (i, l.trim()));
    let span = end.map_or(usize::MAX, |(i, _)| i);
    let want = format!("pcidev: slot {slot} took its first message on vector {vector:#x}");
    if let Some(took) =
        text.lines().take(span).skip(at + 1).find(|l| message(l) == Some(want.as_str()))
    {
        return Ok(Delivery { slot, vector, handed: line.to_string(), took: took.to_string() });
    }

    let mut why = format!(
        "{I219} was handed over on slot {slot}, vector {vector:#x}, and no kernel record \
         `{want}` follows it: the kernel took no message on this function's vector"
    );
    if let Some((_, handed_again)) = end {
        why.push_str(&format!("\n  the slot, handed over again: {handed_again}"));
    }
    for (i, other) in text.lines().enumerate().filter(|(_, l)| l.contains("took its first message")) {
        let said = message(other);
        why.push_str(if said == Some(want.as_str()) && i < at {
            "\n  the record, before the hand-over it would answer: "
        } else if said == Some(want.as_str()) && i > span {
            "\n  the record, after the slot was handed over again: "
        } else if said.is_some_and(|m| {
            m.starts_with("pcidev: slot ") && m.contains(" took its first message on vector 0x")
        }) {
            "\n  another claim's record, not this function's: "
        } else {
            "\n  a line not in the kernel's spelling of the record: "
        });
        why.push_str(other.trim());
    }
    Err(why)
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
            crate::bootlog::declares(&source, &format!("\"{HOSTNAME}\"")),
            "{} declares no constant equal to \"{HOSTNAME}\"",
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

    /// netd's lines and nothing else: a kernel record spelled like netd's,
    /// another program's line quoting netd's head, and a console line with no
    /// head at all are each left out.
    #[test]
    fn only_netds_own_lines_are_netds() {
        let lease = LEASED.split_once("] ").expect("a record").1;
        let log = format!(
            "{{2026-09-08 16:08:23 2.100 netd}} {lease}\n\
             {{2026-09-08 16:08:23 2.200 netd}} netd: ready, at most 8 clients\n\
             {{2026-09-08 16:08:23 2.300 evil}} {{2026-09-08 16:08:23 2.300 netd}} netd: MAC 00:00:00:00:00:00\n\
             [2026-09-08 16:08:23 2.400 cpu0] netd: MAC 00:00:00:00:00:01\n\
             netd: MAC 52:54:00:12:34:56\n"
        );
        let netd = netd_records(&log);
        assert_eq!(netd.lines().count(), 2, "{netd}");
        assert!(lease_in(&netd).is_ok());
        assert!(!netd.contains(MAC), "a line quoting netd was read as netd's: {netd}");
    }

    #[test]
    fn a_link_that_was_already_up_is_told_from_one_that_came_up() {
        let came_up = format!("[x] {LINK_UP}1000 Mb/s, 2400 ms after the driver came up");
        assert_eq!(link_up_ms(&came_up), Ok(2_400));
        assert!(link_up_ms("nothing\n").unwrap_err().contains("never reported a link"));
        let why = link_up_ms(&format!("[x] {LINK_UP}1000 Mb/s")).expect_err("no comma");
        assert!(why.contains("already up"), "{why}");
    }

    /// Metal run 57's `lanicscase` kernel log, verbatim, from the I219's
    /// hand-over to the record of its first message.
    const RUN_57: &str = "\
[2026-09-16 15:01:20 1.333 cpu0] pcidev: PCI 00:1f.6 [8086:15fc] handed over on slot 0, vector 0x28
[2026-09-16 15:01:20 1.360 cpu0] ELF: 3417 relocations indexed (RELATIVE + GLOB_DAT + TPOFF)
[2026-09-16 15:01:20 1.360 cpu0] spawn: TLS 1 modules, total_memsz=144
[2026-09-16 15:01:20 1.518 cpu0] spawn: /system/bin/netd pid=5 tid=0 dst=5 base=0x10000000000 entry=0x1000004e5c0 cr3=0x1cc1000 symbols=2048KiB (layout=25ms relocs=0ms deps=0ms tls=1ms total=184ms)
[2026-09-16 15:01:20 1.552 cpu0] ELF: 2833 relocations indexed (RELATIVE + GLOB_DAT + TPOFF)
[2026-09-16 15:01:20 1.552 cpu0] spawn: TLS 1 modules, total_memsz=144
[2026-09-16 15:01:20 1.739 cpu0] spawn: /system/bin/test-runner pid=6 tid=0 dst=6 base=0x10000000000 entry=0x1000001fff0 cr3=0x1cbf000 symbols=2048KiB (layout=32ms relocs=0ms deps=0ms tls=2ms total=221ms)
[2026-09-16 15:01:20 1.958 cpu4] usb-storage: disk 0 does not implement SYNCHRONIZE CACHE (sense 0x05/0x20/0x00); its writes are durable once they complete
[2026-09-16 15:01:21 2.239 cpu5] shm: 0xa0800000 mapped Uncacheable into pid 5
[2026-09-16 15:01:21 2.239 cpu5] iommu: domain6 maps 0x6800000..0x6a00000 at 0x2000000000
[2026-09-16 15:01:21 2.239 cpu0] pcidev: slot 0 took its first message on vector 0x28
";

    const HANDED: &str = "[2026-09-16 15:01:20 1.333 cpu0] pcidev: PCI 00:1f.6 [8086:15fc] \
                          handed over on slot 0, vector 0x28\n";
    const TOOK: &str =
        "[2026-09-16 15:01:21 2.239 cpu0] pcidev: slot 0 took its first message on vector 0x28\n";

    /// Run 57 with one of its lines replaced, refusing a fixture the edit did
    /// not change.
    fn edited(from: &str, to: &str) -> String {
        assert!(RUN_57.contains(from), "run 57's excerpt carries no {from:?}");
        RUN_57.replacen(from, to, 1)
    }

    #[test]
    fn run_57_took_a_message_on_the_i219s_own_slot_and_vector() {
        let got = delivered(RUN_57).expect("run 57 recorded the message");
        assert_eq!((got.slot, got.vector), (0, 0x28));
        assert_eq!(format!("{}\n", got.handed), HANDED);
        assert_eq!(format!("{}\n", got.took), TOOK);
    }

    #[test]
    fn a_boot_with_no_first_message_record_is_refused_by_the_record_it_lacks() {
        let why = delivered(&edited(TOOK, "")).expect_err("no record");
        assert!(
            why.contains("no kernel record `pcidev: slot 0 took its first message on vector 0x28`"),
            "{why}"
        );
    }

    /// Another writer's line carrying the words is not the kernel's record.
    #[test]
    fn a_line_that_is_not_the_kernels_record_is_refused_and_named() {
        let stray = "[2026-09-16 15:01:21 2.240 cpu3] test-runner: waiting until netd took its \
                     first message\n";
        let log = edited(TOOK, "").replacen(
            "total=184ms)\n",
            &format!("total=184ms)\n{stray}"),
            1,
        );
        assert!(log.contains(stray));
        let why = delivered(&log).expect_err("a userland line");
        assert!(why.contains("no kernel record"), "{why}");
        assert!(why.contains("a line not in the kernel's spelling of the record: "), "{why}");
        assert!(why.contains("test-runner: waiting until netd"), "{why}");
    }

    #[test]
    fn another_slots_record_is_refused_as_another_claims() {
        let why = delivered(&edited(
            TOOK,
            "[2026-09-16 15:01:21 2.239 cpu0] pcidev: slot 1 took its first message on vector \
             0x30\n",
        ))
        .expect_err("slot 1 is not the I219's");
        assert!(why.contains("handed over on slot 0, vector 0x28"), "{why}");
        assert!(why.contains("another claim's record, not this function's: "), "{why}");
        assert!(why.contains("slot 1 took its first message on vector 0x30"), "{why}");
        // The right slot on the wrong vector is not the record either.
        let why = delivered(&edited(TOOK, &TOOK.replace("0x28", "0x29")))
            .expect_err("vector 0x29 is not the one slot 0 was given");
        assert!(why.contains("another claim's record"), "{why}");
    }

    #[test]
    fn a_boot_where_the_i219_was_not_handed_over_is_refused_whatever_else_took_a_message() {
        let log = edited(
            HANDED,
            "[2026-09-16 15:01:20 1.333 cpu0] pcidev: PCI 00:1f.6 [8086:15fc] NOT HANDED OVER: \
             refused\n",
        )
        .replacen(
            TOOK,
            "[2026-09-16 15:01:21 2.239 cpu0] pcidev: slot 3 took its first message on vector \
             0x2a\n",
            1,
        );
        assert!(!log.contains(TOOK));
        let why = delivered(&log).expect_err("the I219 was refused");
        assert!(why.contains("no `[8086:15fc] handed over on slot ` record"), "{why}");
        assert!(why.contains("NOT HANDED OVER"), "{why}");
    }

    #[test]
    fn the_record_before_the_hand_over_answers_nothing() {
        let log = format!("{TOOK}{}", edited(TOOK, ""));
        let why = delivered(&log).expect_err("the record precedes the claim");
        assert!(why.contains("before the hand-over it would answer"), "{why}");
    }

    /// A hand-over of the same slot to another function clears the slot's
    /// record, so a record after it answers that claim and not the I219's.
    #[test]
    fn a_record_after_the_slot_is_handed_over_again_is_refused() {
        let again =
            "[2026-09-16 15:01:21 2.200 cpu0] pcidev: PCI 00:1f.7 [8086:a0f0] handed over on \
             slot 0, vector 0x28\n";
        let log = edited(TOOK, &format!("{again}{TOOK}"));
        let why = delivered(&log).expect_err("the record answers the later claim");
        assert!(why.contains("the slot, handed over again: "), "{why}");
        assert!(why.contains("[8086:a0f0] handed over on slot 0"), "{why}");
        assert!(why.contains("the record, after the slot was handed over again: "), "{why}");
        // Another slot's hand-over ends nothing.
        let other = again.replace("slot 0, vector 0x28", "slot 1, vector 0x29");
        assert!(delivered(&edited(TOOK, &format!("{other}{TOOK}"))).is_ok());
    }

    /// Another writer's line carrying the hand-over's words is not the kernel's
    /// hand-over.
    #[test]
    fn a_hand_over_not_in_the_kernels_spelling_is_refused() {
        let stray = "[2026-09-16 15:01:20 1.333 cpu0] netd: [8086:15fc] handed over on slot 0, \
                     vector 0x28\n";
        let why = delivered(&edited(HANDED, stray)).expect_err("a userland line");
        assert!(why.contains("no `[8086:15fc] handed over on slot ` record"), "{why}");
    }

    /// The string literal whose text begins `head`, as the compiler reads it:
    /// a `\` at a line's end joins the next line with its indent dropped.
    fn literal(source: &str, head: &str) -> Option<String> {
        let (_, rest) = source.split_once(&format!("\"{head}"))?;
        let (body, _) = rest.split_once('"')?;
        let mut out = head.to_string();
        let mut lines = body.split("\\\n");
        out.push_str(lines.next()?);
        for line in lines {
            out.push_str(line.trim_start());
        }
        Some(out)
    }

    /// Nothing links the kernel to the build system, so the two records this
    /// judge reads are held to the kernel's own format strings, whole.
    #[test]
    fn the_kernel_writes_both_records_in_the_spelling_read_here() {
        let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("kernel/src/pcidev/mod.rs");
        let source = std::fs::read_to_string(&at).expect("the kernel's pcidev module");
        for (head, whole) in [
            (
                "pcidev: PCI ",
                "pcidev: PCI {:02x}:{:02x}.{} [{:04x}:{:04x}] handed over on slot {slot}, \
                 vector {:#x}",
            ),
            ("pcidev: slot ", "pcidev: slot {slot} took its first message on vector {:#x}"),
        ] {
            let spelled: Vec<String> = source
                .match_indices(&format!("\"{head}"))
                .filter_map(|(i, _)| literal(&source[i..], head))
                .collect();
            assert!(
                spelled.iter().any(|l| l == whole),
                "{} spells no {whole:?}; its {head:?} literals are {spelled:#?}",
                at.display()
            );
        }
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
