//! The cable: netd taking this machine's address from the network, and the T14
//! answering the development host on it.
//!
//! Every line read here is a record, or the one file netd leaves beside them, the
//! lease probe's report. On the T14 a userland `println!` reaches no serial
//! port and crosses to the stick as a record in the form only a program's line
//! takes; the judge reads netd's by that form and no other program's.

use std::net::Ipv4Addr;
use std::path::Path;

use toyos_build::bootlog;
use toyos_build::lan::{
    asked_under_its_own_name, lease_in, link_up_ms, Lease, HOSTNAME, LEASE, LINK_UP, MAC, NO_LEASE,
    READY,
};
use toyos_build::metaldevices;
use toyos_build::metalprofile::Profile;
use toyos_i219::lease::{self, Event, Verdict};
use toyos_i219::phy::PhyRefusal;

use super::metal;
use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

/// The boot config the T14 arm flashes, and the name every profile row for that
/// boot is under.
pub const CONFIG: &str = "tests/lancase";
pub const BOOT: &str = "lancase";

/// The same boot with netd's `--provoke-message` armed: the arm that says
/// whether a message the card raises reaches a CPU at all, which no reading of
/// the shipping boot separates from a card that raised none.
pub const ICS_CONFIG: &str = "tests/lanicscase";
pub const ICS_BOOT: &str = "lanicscase";

/// The same boot with netd's `--exit-with-lease` armed: netd brings the card up
/// and serves as that boot does, leaves [`LEASE_FILE`] on the log volume one
/// durable line at a time, and ends with the lease's verdict as its exit code,
/// which the kernel's `exit:` record carries off a machine whose console
/// reaches nobody.
pub const LEASE_CONFIG: &str = "tests/lanleasecase";
pub const LEASE_BOOT: &str = "lanleasecase";

/// The file that report is left in, at the root of the log volume — netd's
/// `report::PATH` under `/log`.
pub const LEASE_FILE: &str = "lease.txt";

/// netd, as the kernel's `exit:` record names it.
const NETD: &str = "netd";

/// The one job on that boot: it holds the machine up while the host pings it.
pub const JOBS: &[&str] = &["test_rs_lan_hold"];

/// The boot the host talks to over its own cable: the record stream, sshd, and
/// `reboot` as the way the machine is handed back.
pub const TALK_CONFIG: &str = "tests/lantalkcase";
pub const TALK_BOOT: &str = "lantalkcase";

/// Its one job holds the machine until the runner's bound is near, as the
/// fallback for a host that never tells it to reboot.
const TALK_HOLD: &str = "test_rs_lan_talk_hold";
pub const TALK_JOBS: &[&str] = &[TALK_HOLD];

/// The same boot in front of QEMU's 82574, and the key its image authorizes.
const TALK_QEMU_CONFIG: &str = "tests/e1000talkcase";
const TALK_KEY: &str = "lantalk";

/// A liveness guard on a rehearsal guest that never opened its stream, never a
/// verdict.
const TALK_CEILING: std::time::Duration = std::time::Duration::from_secs(120);

/// The armed boot's judge: the kernel's own records, tied to the I219's
/// hand-over, say whether a message it raised reached a CPU — whatever the PHY
/// did about a link.
pub fn provoked_on_metal(back: &metal::Readback) -> Result<(), String> {
    let got = toyos_build::lan::delivered(back.kernel().text())?;
    eprintln!("  [lan] {}", got.handed.trim());
    eprintln!("  [lan] {}", got.took.trim());
    Ok(())
}

/// The config the QEMU arm boots — the Intel driver in front of the user-mode
/// backend, which is the same driver the T14 arm runs and the only DHCP server
/// this host can put in front of it.
const QEMU_CONFIG: &str = "tests/e1000case";

/// The same, with netd's `--exit-with-lease` armed.
const LEASE_QEMU_CONFIG: &str = "tests/e1000leasecase";

/// What QEMU's user-mode backend leases, and what it says about the network it
/// leases on. Its own defaults, not this repository's: they are the oracle.
const SLIRP_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
const SLIRP_PREFIX: u8 = 24;
const SLIRP_ROUTER: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
const SLIRP_DNS: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 3);

/// The card the T14 arm claims, as the kernel and the manifest spell it.
const ID: &str = "8086:15fc";

/// The PCI function that card is, as `/sys/bus/pci/devices` spells it: the
/// cable the metal loop reaches this boot over while it runs.
pub const NIC: &str = "0000:00:1f.6";

/// The T14's judge: the claim, the card, the lease, and the host's own ping.
pub fn on_metal(back: &metal::Readback) -> Result<(), String> {
    let profile = Profile::load(&super::compile::repo_root()).map_err(|why| why.to_string())?;
    let kernel = back.kernel();
    let text = kernel.text();
    let mut bad: Vec<String> = Vec::new();
    let cable = back.cable.as_ref().ok_or_else(|| {
        format!(
            "{}'s readback carries no cable: this boot was driven by a loop that was not asked \
             to reach it over one, so nothing here is about the network",
            back.label
        )
    })?;

    // A boot with no hand-over line carries the kernel's refusal instead, and
    // quoting that is the whole diagnosis.
    let handed = format!("[{}] handed over on slot", ID);
    match text.lines().find(|l| l.contains(&handed)) {
        Some(line) => eprintln!("  [lan] {}", line.trim()),
        None => bad.push(match text.lines().find(|l| l.contains("NOT HANDED OVER")) {
            Some(line) => format!("the kernel refused this function: {}", line.trim()),
            None => format!(
                "no `{handed}` record and no refusal either: nothing on this machine claimed \
                 {ID}, so `tests/lancase` was flashed onto a machine that has no such card"
            ),
        }),
    }

    // netd's own records, in the form the kernel gives a program's, under netd's tag.
    let log = back.log();
    let netd = toyos_build::lan::netd_records(log.text());
    for owed in [MAC, LINK_UP, READY] {
        if !netd.contains(owed) {
            bad.push(format!("no {owed:?} record"));
        }
    }

    let mac = format!("{MAC}{}", cable.mac);
    if !netd.contains(&mac) {
        bad.push(format!(
            "no {mac:?} record: the card this boot brought up is not the one that held {} \
             before it",
            cable.addr
        ));
    }

    match link_up_ms(&netd) {
        Ok(ms) => {
            eprintln!("  [lan] the link came up {ms} ms after the driver did");
            if let Err(why) = profile.judge(&format!("lan.{}.link_up_ms", back.label), ms) {
                bad.push(why.to_string());
            }
        }
        Err(why) => bad.push(why),
    }

    match lease_in(&netd) {
        Ok(lease) => {
            eprintln!(
                "  [lan] leased {}/{} from {} in {} ms, gateway {}, dns {:?}",
                lease.address, lease.prefix, lease.server, lease.ms, lease.gateway, lease.dns
            );
            if let Err(why) = profile.judge(&format!("lan.{}.lease_ms", back.label), lease.ms) {
                bad.push(why.to_string());
            }
            if lease.address != cable.addr {
                bad.push(format!(
                    "this boot leased {} and the host pinged {}, which the router hands this \
                     MAC under the operating system before it — so either something else \
                     answered or that server does not repeat a lease across the two",
                    lease.address, cable.addr
                ));
            }
        }
        Err(why) => bad.push(why),
    }

    match cable.reply {
        Some(reply) => {
            eprintln!(
                "  [lan] {} answered the host's ping {} s into the window",
                cable.addr, reply.secs
            );
            // The bracket first: a reply from the operating system on the other
            // side of the reset is not this boot's reading, and a ceiling may
            // only be tightened against a reading this boot answered.
            match bootlog::host_second_inside_this_boot(log.text(), cable.skew, LEASE, reply.at) {
                Ok(()) => {
                    if let Err(why) =
                        profile.judge(&format!("boot.{}.ping_secs", back.label), reply.secs)
                    {
                        bad.push(why.to_string());
                    }
                }
                Err(why) => bad.push(why),
            }
        }
        None => bad.push(format!(
            "nothing answered a ping at {} while this machine was between its two operating \
             systems",
            cable.addr
        )),
    }

    if let Err(why) = back.job_passed(JOBS[0]) {
        bad.push(why);
    }

    if bad.is_empty() {
        return Ok(());
    }
    Err(format!("{} finding(s):\n  {}", bad.len(), bad.join("\n  ")))
}

/// The lease probe's judge: netd's exit code, decoded through the table the
/// driver crate owns, and the report it left on the log volume — the lease,
/// the router, and what the driver and the MAC counted each way. A code that is
/// no lease is a finding by its name, which the shipping boot's silence cannot
/// give.
///
/// **The host's ping is printed and not judged**: the lease is a server this
/// machine does not control answering it, which is the claim; a reply at the
/// leased address inside this boot is the same claim made from the bench's
/// side, and the loop asks for it only where it was told the cable.
pub fn leased_on_metal(back: &metal::Readback) -> Result<(), String> {
    let code = back.exit_code(NETD)?;
    let text = back
        .log_volume_file(LEASE_FILE)?
        .ok_or_else(|| format!("{LEASE_BOOT}'s log volume carries no {LEASE_FILE}"))?;
    for line in text.lines() {
        eprintln!("  [lan] {LEASE_FILE}: {line}");
    }
    let summary = lease::summary(&text).map_err(|why| format!("{LEASE_FILE}: {why}"))?;
    if summary.exit != Some(code) {
        return Err(format!(
            "netd exited {code} and its report ends in {:?}: the two records of one exit disagree",
            summary.exit
        ));
    }
    match Verdict::from_exit_code(code) {
        Some(Verdict::Leased) => {}
        Some(other) => return Err(format!("netd exited {code} on {LEASE_BOOT}: {other}")),
        None => {
            return Err(format!("netd exited {code} on {LEASE_BOOT}, which is no verdict the probe encodes"))
        }
    }
    let Some((ms, Event::Leased { address, prefix, server, router })) = summary.lease else {
        return Err(format!("netd exited leased and {LEASE_FILE} records no lease"));
    };
    let counts = summary.counts.ok_or_else(|| format!("{LEASE_FILE} carries no counts"))?;
    eprintln!(
        "  [lan] leased {address}/{prefix} from {server}, router {router:?}, {ms} ms after netd \
         started; the driver counted {} sent and {} received, the MAC {} sent, {} received of \
         {} seen",
        counts.sent, counts.received, counts.wire.sent, counts.wire.received, counts.wire.seen
    );
    if counts.sent == 0 || counts.received == 0 {
        return Err(format!("a lease with {counts:?} is no exchange this card carried"));
    }
    match back.cable.as_ref() {
        None => eprintln!("  [lan] no cable was named, so the host asked nothing of this boot"),
        Some(cable) => match cable.reply {
            None => eprintln!("  [lan] nothing answered the host's ping at {}", cable.addr),
            Some(reply) => {
                let handed = format!("[{ID}] handed over on slot");
                match bootlog::host_second_inside_this_boot(
                    back.kernel().text(),
                    cable.skew,
                    &handed,
                    reply.at,
                ) {
                    Ok(()) => eprintln!(
                        "  [lan] {} answered the host's ping {} s into the window, inside this \
                         boot",
                        cable.addr, reply.secs
                    ),
                    Err(why) => eprintln!(
                        "  [lan] {} answered the host's ping, and not inside this boot: {why}",
                        cable.addr
                    ),
                }
            }
        },
    }
    Ok(())
}

/// The netdev QEMU's `e1000e` profile names its backend, which the monitor's
/// `set_link` is addressed to.
const FLAP_NETDEV: &str = "net0";

/// netd's own line for a pass that found the link gone, which the link is
/// kept away until: the pass that says it is the one that records the down.
const LINK_DOWN: &str = "netd: I219: link down";

/// The report of a boot whose link was taken away after its lease: the link
/// goes down and comes back up after the first lease, and neither a `lost` nor
/// a second `leased` line follows it — the client never started over, which
/// is what gives the address up. Against QEMU's server the second is the one
/// that shows: a restart is answered inside the pass that made it, so the loss
/// between the two never reaches the report.
fn flap_kept_the_lease(text: &str) -> Result<(), String> {
    let events: Vec<Event> =
        text.lines().filter_map(lease::Line::parse).map(|line| line.event).collect();
    let leased = events
        .iter()
        .position(|event| matches!(event, Event::Leased { .. }))
        .ok_or_else(|| format!("the report records no lease:\n{text}"))?;
    let after = &events[leased..];
    let down = after
        .iter()
        .position(|event| *event == Event::Link(toyos_i219::Link::Down))
        .ok_or_else(|| format!("the report never saw the link go down after its lease:\n{text}"))?;
    if !after[down..].iter().any(|event| matches!(event, Event::Link(toyos_i219::Link::Up { .. }))) {
        return Err(format!("the report never saw the link come back:\n{text}"));
    }
    if after.contains(&Event::Lost) {
        return Err(format!("the lease was given up across the flap:\n{text}"));
    }
    if after[1..].iter().any(|event| matches!(event, Event::Leased { .. })) {
        return Err(format!("the client started over across the flap:\n{text}"));
    }
    Ok(())
}

/// The lease probe, end to end, in front of QEMU's 82574 and its user-mode
/// DHCP server: netd serves its window, the kernel's `exit:` record carries the
/// verdict, and the report read back out of the image by the host's own FAT
/// implementation names the lease that server hands out, field by field, with
/// frames counted both ways by the driver and by the MAC's statistics —
/// which QEMU's model keeps, and not this repository.
///
/// **The link is taken away and given back once the lease has landed**, from
/// QEMU's own monitor, and the lease has to outlive it: the report says the
/// link went down and came up after the lease, never that the lease was lost,
/// and the verdict is a lease held at the end of the window.
pub fn lan_lease_report(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let case = super::compile::repo_root().join(LEASE_QEMU_CONFIG);
    let image_path = super::lane::dir().join("lan-lease-report.img");
    let image = qemu::build_boot_image(&case, &[], &[], &[]);
    std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;
    let (start, len) = super::volumes::log_extent(&image, &image_path)?;

    let options = BootOptions {
        profile: qemu::Profile::E1000e,
        boot_image: Some(qemu::Staged::Written(image_path.clone())),
        qmp: true,
        ..Default::default()
    };
    if !qemu::profile_argv(&options).iter().any(|a| a.contains("e1000e")) {
        return Err("this test needs an Intel NIC and the profile has none".to_string());
    }
    let mut guest = QemuInstance::boot_with_options(&case, &[], &[], options);
    let mut console = guest.boot_log().to_string();
    // netd says it is ready once the lease is applied, which is when a flap is
    // one a bound lease has to survive.
    qemu::await_marker(&mut guest, &mut console, READY, "netd to take its lease")?;
    {
        let mut monitor = qemu::QmpMonitor::open(guest.qmp_socket());
        let mut set_link = |state: &str| {
            let said = monitor.human(&format!("set_link {FLAP_NETDEV} {state}"));
            match said.trim().is_empty() {
                true => Ok(()),
                false => Err(format!("QEMU's monitor refused `set_link {state}`: {said}")),
            }
        };
        let from = console.len();
        set_link("off")?;
        qemu::await_marker_new(&mut guest, &mut console, LINK_DOWN, from, "netd to see the link go down")?;
        set_link("on")?;
    }
    // Drained rather than waited on: once the lease lands netd says nothing
    // until its window ends, and every wait in this harness reads a quiet guest
    // as one that stopped. The window ends inside this drain.
    console.push_str(
        &guest.drain_serial(std::time::Duration::from_millis(toyos_tco::LEASE_BOUND_MS)),
    );
    let exited = format!("{}{NETD} pid=", bootlog::EXIT);
    qemu::await_marker(&mut guest, &mut console, &exited, "netd to end its lease probe")?;
    drop(guest);
    let code = metaldevices::exit_of(&console, NETD)
        .and_then(|exit| i32::try_from(exit.code).ok())
        .ok_or_else(|| format!("no readable `{exited}` record:\n{console}"))?;
    let log = serial::Serial::named("the lan lease boot", console.as_str());
    log.must_be_clean()?;
    if Verdict::from_exit_code(code) != Some(Verdict::Leased) {
        return Err(format!(
            "netd exited {code} on the 82574, which the table reads as {:?} and not a lease",
            Verdict::from_exit_code(code)
        ));
    }

    let after = std::fs::read(&image_path).map_err(|e| format!("read the image back: {e}"))?;
    let volume = after.get(start..start + len).ok_or("the image shrank under the log partition")?;
    let text = super::volumes::read_files(volume, &[LEASE_FILE])?
        .pop()
        .flatten()
        .ok_or_else(|| format!("the log volume carries no {LEASE_FILE}"))?;
    let text = String::from_utf8(text).map_err(|e| format!("{LEASE_FILE}: {e}"))?;
    let complaints = toyos_fat32_check::check(volume);
    if !complaints.is_empty() {
        return Err(format!(
            "the report gave the checker something to say about the log volume:\n{}",
            toyos_fat32_check::describe(&complaints)
        ));
    }
    let _ = std::fs::remove_file(&image_path);

    let summary = lease::summary(&text).map_err(|why| format!("{LEASE_FILE}: {why}\n{text}"))?;
    if summary.exit != Some(code) {
        return Err(format!("netd exited {code} and its report ends {:?}:\n{text}", summary.exit));
    }
    let want = Event::Leased {
        address: SLIRP_ADDRESS,
        prefix: SLIRP_PREFIX,
        server: SLIRP_ROUTER,
        router: Some(SLIRP_ROUTER),
    };
    match summary.lease {
        Some((_, got)) if got == want => {}
        other => return Err(format!("the report's lease is {other:?} and the backend serves {want:?}:\n{text}")),
    }
    let counts = summary.counts.ok_or_else(|| format!("the report carries no counts:\n{text}"))?;
    if counts.sent == 0 || counts.received == 0 || counts.wire.sent == 0 || counts.wire.received == 0 {
        return Err(format!("a lease with {counts:?} is no exchange both counts saw:\n{text}"));
    }
    if !text.lines().any(|line| line.ends_with(" link up 1000 full")) {
        return Err(format!("the report never says the emulated link came up:\n{text}"));
    }
    flap_kept_the_lease(&text)?;
    if !summary.held {
        return Err(format!("netd exited leased and its report ends without a lease:\n{text}"));
    }
    eprintln!("  [lan] netd exited {code}, a lease; its report:");
    for line in text.lines() {
        eprintln!("  [lan]   {line}");
    }
    Ok(())
}

/// The QEMU arm: the client, against a DHCP server this repository did not
/// write.
///
/// Every field of the lease is checked, because a client that dropped the router
/// option or read the mask off the wrong one would otherwise pass where the
/// answers happen to agree; and the readiness line is checked to come *after*
/// the lease, because every other arm waits for it and then connects.
pub fn lan_dhcp_lease(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let case = super::compile::repo_root().join(QEMU_CONFIG);
    let dump = wire_dump();
    let options = BootOptions {
        profile: qemu::Profile::E1000e,
        wire_dump: Some(dump.clone()),
        ..Default::default()
    };
    if !qemu::profile_argv(&options).iter().any(|a| a.contains("e1000e")) {
        return Err("this test needs an Intel NIC and the profile has none".to_string());
    }
    let mut guest = QemuInstance::boot_with_options(&case, &[], &[], options);
    let mut console = guest.boot_log().to_string();
    qemu::await_marker(&mut guest, &mut console, READY, "netd to take an address")?;
    console.push_str(&guest.drain_serial(std::time::Duration::from_millis(500)));
    // QEMU owns the pcap while it runs, and every refusal below is a return:
    // the frames are taken once the machine is gone and the file removed here.
    drop(guest);
    let frames = std::fs::read(&dump).map_err(|e| format!("{}: {e}", dump.display()))?;
    let _ = std::fs::remove_file(&dump);
    let log = serial::Serial::named("the lan boot", console.as_str());

    let lease = lease_in(log.text())?;
    let want = Lease {
        address: SLIRP_ADDRESS,
        prefix: SLIRP_PREFIX,
        server: SLIRP_ROUTER,
        gateway: SLIRP_ROUTER,
        dns: vec![SLIRP_DNS],
        ms: lease.ms,
    };
    if lease != want {
        return Err(format!(
            "the client read this lease as {lease:?} and the backend serves {want:?}"
        ));
    }
    // The only thing the I219's §9 bring-up may say on the 82574 this host
    // emulates, in the driver crate's own words.
    log.must_say(&PhyRefusal::NotThisRegisterMap.to_string())?;
    // The order, and not merely the presence of both.
    log.must_say_after(LEASE, READY)?;
    log.must_say(LINK_UP)?;
    let ms = link_up_ms(log.text())?;
    eprintln!(
        "  [lan] the emulated link came up in {ms} ms and the lease landed {} ms after netd \
         started",
        lease.ms
    );
    log.must_be_clean()?;
    asked_under_its_own_name(&frames)?;
    eprintln!("  [lan] the client asked under its own name on the wire");
    Ok(())
}

/// The client on a wire with nothing at the other end.
///
/// **The refusal the lease boot cannot reach.** A machine whose network never
/// answers still has to announce itself, or every arm that waits for that line
/// hangs instead of having its connects refused one at a time.
pub fn lan_no_lease(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let case = super::compile::repo_root().join(QEMU_CONFIG);
    let options = BootOptions { profile: qemu::Profile::E1000eNoServer, ..Default::default() };
    let mut guest = QemuInstance::boot_with_options(&case, &[], &[], options);
    let mut console = guest.boot_log().to_string();
    // Drained rather than waited on: netd owes its line inside its own bound
    // and the guest says nothing at all until then, which every wait in this
    // harness reads as a machine that stopped.
    console.push_str(
        &guest.drain_serial(std::time::Duration::from_millis(toyos_tco::LEASE_BOUND_MS + 10_000)),
    );
    let log = serial::Serial::named("the lan boot with no server", console.as_str());
    if let Ok(lease) = lease_in(log.text()) {
        return Err(format!("a wire with no server leased {lease:?}"));
    }
    log.must_say_after(&format!("{NO_LEASE}{HOSTNAME} in "), READY)?;
    eprintln!("  [lan] no server answered and netd said so, then served anyway");
    Ok(())
}

/// The T14 talked to over its own cable, judged from the Mac's side: the
/// record stream arrived while the machine booted and is the stick's own log
/// in its own order, the address that opened it answered a ping and ran the
/// command, and `reboot` handed the machine back — before its hold would have.
pub fn talked_on_metal(back: &metal::Readback) -> Result<(), String> {
    let (heard, stream) = back.talk()?;
    let mut bad: Vec<String> = Vec::new();
    match toyos_build::metaltalk::judge(&heard, &stream) {
        Ok(said) => said.iter().for_each(|line| eprintln!("  [talk] {line}")),
        Err(found) => bad.extend(found),
    }
    // **The stick is the oracle the wire is compared with**: `logd` writes a
    // line to `/log` and then offers it to the stream, so what arrived is the
    // file's own lines in the file's own order, with holes only where the
    // queue refused one — and the file came back over a different path.
    let file: Vec<String> =
        back.kernel().text().split_inclusive('\n').map(str::to_string).collect();
    match super::logstream::is_subsequence_of(&stream, &file) {
        Ok(()) => eprintln!(
            "  [talk] the {} streamed record(s) are the stick's own, in its order ({} in the file)",
            stream.len(),
            file.len()
        ),
        Err(why) => bad.push(why),
    }
    // The command and not the hold ended this boot: a hold that exited 0 is a
    // machine that waited out its own bound.
    match back.exit_code(TALK_HOLD) {
        Ok(0) => bad.push(format!(
            "{TALK_HOLD} exited 0, so the boot ran to its own hold and `reboot` over ssh was \
             not what ended it"
        )),
        Ok(code) => eprintln!("  [talk] {TALK_HOLD} was ended under the reboot ({code})"),
        Err(_) => eprintln!("  [talk] {TALK_HOLD} left no exit record: the reboot ended it"),
    }
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!("{} finding(s):\n  {}", bad.len(), bad.join("\n  ")))
}

/// The talking boot rehearsed in front of QEMU's 82574: the host's listener
/// receives the boot's records while it boots, the same conversation the metal
/// loop has goes through slirp's forward to sshd, and `reboot` over it ends the
/// guest. The stream is then compared with the guest's own `/log`, read off the
/// volume behind its back.
///
/// **What this cannot rehearse is the network**: slirp answers no ICMP from the
/// host and translates the stream's peer, so the ping and the peer's address
/// are the T14's to judge.
pub fn lan_talk(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    use toyos_build::metaltalk::{self, Ssh};

    let root = super::compile::repo_root();
    let staged = TalkBoot::stage("lan-talk")?;
    let (stream, scratch) = (&staged.stream, &staged.scratch);
    let ssh_port = qemu::free_host_port();
    let options = BootOptions { ssh_port: Some(ssh_port), ..staged.options() };
    let mut guest = QemuInstance::boot_with_options(&staged.case, &[], &[], options);
    let mut console = guest.boot_log().to_string();

    let ssh = Ssh::at(&root, staged.identity.private().to_path_buf())?;
    let forward = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, ssh_port));
    let conversation =
        metaltalk::converse(stream, &ssh, Some(forward), false, TALK_CEILING, scratch)?;
    // `-no-reboot`: the guest's own reset ends QEMU, and its last word is
    // the kernel's.
    qemu::await_marker(&mut guest, &mut console, bootlog::REBOOTING, "`reboot` over ssh")?;
    drop(guest);
    serial::Serial::named("the talking boot", console.as_str()).must_be_clean()?;

    let heard = metaltalk::Conversation::parse(&conversation.render())?
        .ok_or("a rendered conversation names its peer")?;
    let said = metaltalk::judge(&heard, &stream.lines())
        .map_err(|bad| format!("{} finding(s):\n  {}", bad.len(), bad.join("\n  ")))?;
    let file = super::volumes::whole_log(&staged.image, staged.start, staged.len)?;
    super::logstream::is_subsequence_of(&stream.lines(), &file)?;
    for line in said {
        eprintln!("  [talk] {line}");
    }
    eprintln!(
        "  [talk] the {} streamed record(s) are /log's own, in its order ({} in the file)",
        stream.lines().len(),
        file.len()
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// The talking boot's image, streaming to `at`, authorizing `identity` and
/// arming `actuators`, and where its log partition sits in it.
fn talk_image(
    case: &Path,
    name: &str,
    at: (&'static str, u16),
    identity: &super::ssh::Identity,
    actuators: &[&str],
) -> Result<(std::path::PathBuf, usize, usize), String> {
    let param = qemu::log_stream_param(at);
    // In `BootOptions::params`'s order, which the staged image is held to.
    let mut params: Vec<&str> = actuators.to_vec();
    params.push(&param);
    let bytes = qemu::build_boot_image_carrying(
        case,
        &[],
        &[],
        &[(super::ssh::KEYS_ON_ROOT.to_string(), identity.authorized_line().into_bytes())],
        &params,
    );
    let image = super::lane::dir().join(format!("{name}.img"));
    std::fs::write(&image, &bytes).map_err(|e| format!("write {}: {e}", image.display()))?;
    let (start, len) = super::volumes::log_extent(&bytes, &image)?;
    Ok((image, start, len))
}

/// A talking boot staged in front of one of QEMU's NICs: its listener, the key
/// its image authorizes, and where its log partition sits in the image.
pub(super) struct TalkBoot {
    pub(super) case: std::path::PathBuf,
    pub(super) stream: toyos_build::metaltalk::Stream,
    pub(super) identity: super::ssh::Identity,
    pub(super) image: std::path::PathBuf,
    pub(super) scratch: std::path::PathBuf,
    at: (&'static str, u16),
    bench: super::logstream::Bench,
    actuators: &'static [&'static str],
    pub(super) start: usize,
    pub(super) len: usize,
}

/// The talking boot's NIC: QEMU's 82574, the part whose register file the
/// T14's I219 has.
pub(super) const TALK_BENCH: super::logstream::Bench = super::logstream::Bench {
    profile: qemu::Profile::E1000e,
    config: TALK_QEMU_CONFIG,
    device: "e1000e",
};

impl TalkBoot {
    fn stage(name: &str) -> Result<Self, String> {
        Self::stage_on(name, TALK_BENCH)
    }

    /// `bench.config`'s boot staged to stream to a listener of this host's and
    /// to authorize the lane's talking key.
    pub(super) fn stage_on(name: &str, bench: super::logstream::Bench) -> Result<Self, String> {
        Self::stage_armed(name, bench, &[])
    }

    /// [`TalkBoot::stage_on`] on the test kernel, with `actuators` armed.
    pub(super) fn stage_armed(
        name: &str,
        bench: super::logstream::Bench,
        actuators: &'static [&'static str],
    ) -> Result<Self, String> {
        let case = super::compile::repo_root().join(bench.config);
        let scratch = super::lane::dir().join(name);
        std::fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
        let identity = super::ssh::Identity::mint(TALK_KEY)?;
        let stream = toyos_build::metaltalk::Stream::listen(
            std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            &scratch.join(toyos_build::metal::READBACK_STREAM),
            false,
        )?;
        let at = (qemu::GUEST_VIEW_OF_HOST, stream.local().port());
        let (image, start, len) = talk_image(&case, name, at, &identity, actuators)?;
        Ok(Self { case, stream, identity, image, scratch, at, bench, actuators, start, len })
    }

    /// The boot's options, the NIC asked of the argv rather than assumed.
    pub(super) fn options(&self) -> BootOptions {
        let options = BootOptions {
            profile: self.bench.profile,
            boot_image: Some(qemu::Staged::Written(self.image.clone())),
            log_stream: Some(self.at),
            kernel_params: self.actuators,
            ..Default::default()
        };
        assert!(
            qemu::profile_argv(&options).iter().any(|a| a.contains(self.bench.device)),
            "[lan] this boot needs {} and the profile has none",
            self.bench.device
        );
        options
    }
}

/// The marker a late-link boot takes its cable out at: `logd` exists and netd
/// does not yet, so no lease can have landed.
const LOGD_SPAWNED: &str = "spawn: /system/bin/logd ";

/// How long the late-link boot's cable stays out: a stimulus's pace, never a
/// verdict — long enough that `logd` asks through a machine with no address
/// many times over, which is what the T14's slow PHY gives it.
const CABLE_OUT: std::time::Duration = std::time::Duration::from_secs(8);

/// **The network comes up late, and the stream still opens.** The talking boot
/// with its cable taken out before netd starts and put back seconds later. On
/// the T14 the lease lands seconds after `logd` first asks for its stream, and
/// a machine with no address yet answered that ask as a peer's refusal, which
/// `logd` took as final. The premise is asked of the guest's own console — no
/// lease before the cable goes back — and the verdict is the stream opening
/// and carrying the boot's `Boot: complete`.
pub fn lan_talk_late_link(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let staged = TalkBoot::stage("lan-talk-late-link")?;
    let options = BootOptions { qmp: true, ready_marker: LOGD_SPAWNED, ..staged.options() };
    let mut guest = QemuInstance::boot_with_options(&staged.case, &[], &[], options);
    let mut qmp = qemu::QmpDevices::open(guest.qmp_socket());
    qmp.set_link("net0", false);
    let mut console = guest.boot_log().to_string();
    console.push_str(&guest.drain_serial(CABLE_OUT));
    if console.contains(LEASE) {
        return Err(format!(
            "the premise did not hold: the guest leased before its cable went out, so nothing \
             here came up late\n{console}"
        ));
    }
    if let Some(peer) = staged.stream.peer() {
        return Err(format!("the stream opened from {peer} with the cable out"));
    }
    qmp.set_link("net0", true);
    drop(qmp);
    let opened = staged.stream.wait_connected(TALK_CEILING);
    console.push_str(&guest.drain_serial(std::time::Duration::from_millis(500)));
    drop(guest);
    if opened.is_none() {
        return Err(format!(
            "the cable went back and the stream never opened in {} s\n{console}",
            TALK_CEILING.as_secs()
        ));
    }
    let lines = staged.stream.lines();
    if bootlog::boot_millis(&lines.concat()).is_none() {
        return Err(format!("the stream opened and carries no `Boot: complete`: {lines:?}"));
    }
    if !console.contains(LEASE) {
        return Err(format!("the stream opened and the guest never said it leased\n{console}"));
    }
    serial::Serial::named("the late-link boot", console.as_str()).must_be_clean()?;
    eprintln!(
        "  [talk] the cable went out before netd, back {} s later, and the stream opened with {} \
         record(s), `Boot: complete` among them",
        CABLE_OUT.as_secs(),
        lines.len()
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// Where this process writes the frames one boot put on its wire.
fn wire_dump() -> std::path::PathBuf {
    let at = std::env::temp_dir().join(format!("toyos-lan-{}.pcap", std::process::id()));
    let _ = std::fs::remove_file(&at);
    at
}

/// **The T14's first talking boot to open its stream, on the 82574**: this
/// host accepts the boot's connection and closes it before reading a byte —
/// what macOS's application firewall does to a binary it blocks incoming
/// connections for. The boot goes on, and what `logd` owes is to notice: the
/// stream it was writing into is gone, and `/log` says so.
pub fn lan_talk_host_closes(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let case = super::compile::repo_root().join(TALK_QEMU_CONFIG);
    let identity = super::ssh::Identity::mint(TALK_KEY)?;
    let closer = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|e| format!("bind the closing listener: {e}"))?;
    let port = closer.local_addr().map_err(|e| format!("its port: {e}"))?.port();
    let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&accepted);
    std::thread::spawn(move || {
        for conn in closer.incoming() {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            drop(conn);
        }
    });
    let at = (qemu::GUEST_VIEW_OF_HOST, port);
    let (image, start, len) = talk_image(&case, "lan-talk-host-closes", at, &identity, &[])?;
    let options = BootOptions {
        profile: qemu::Profile::E1000e,
        boot_image: Some(qemu::Staged::Written(image.clone())),
        log_stream: Some(at),
        ..Default::default()
    };
    let mut guest = QemuInstance::boot_with_options(&case, &[], &[], options);
    let mut console = guest.boot_log().to_string();
    // A pace for records to be offered into a connection that is gone, never
    // a verdict: the verdict is what the guest's own log says.
    console.push_str(&guest.drain_serial(std::time::Duration::from_secs(10)));
    drop(guest);
    serial::Serial::named("the host-closes boot", console.as_str()).must_be_clean()?;
    let times = accepted.load(std::sync::atomic::Ordering::SeqCst);
    if times == 0 {
        return Err("the boot never connected, so nothing here was closed on it".to_string());
    }
    let file = super::volumes::whole_log(&image, start, len)?;
    let said = format!("the log stream to {}:{port}", qemu::GUEST_VIEW_OF_HOST);
    let Some(line) = file.iter().find(|l| l.contains(&said)) else {
        return Err(format!(
            "this host closed the boot's stream {times} time(s) and /log never says so; it ends \
             {:?}",
            file.iter().rev().take(5).collect::<Vec<_>>()
        ));
    };
    eprintln!("  [talk] this host closed the stream on accept ({times} time(s)); /log: {}", line.trim_end());
    let _ = std::fs::remove_file(&image);
    Ok(())
}
