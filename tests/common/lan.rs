//! The cable: netd taking this machine's address from the network, and the T14
//! answering the development host on it.
//!
//! Every line read here is a record. On the T14 a userland `println!` reaches
//! `Backend::None`, so what crosses to the stick is the kernel's log — and the
//! one file netd leaves beside it, the crumb trail.

use std::net::Ipv4Addr;
use std::path::Path;

use toyos_build::bootlog;
use toyos_build::lan::{
    asked_under_its_own_name, lease_in, link_up_ms, Lease, HOSTNAME, LEASE, LINK_UP, MAC, NO_LEASE,
    READY,
};
use toyos_build::metaldevices;
use toyos_build::metalprofile::Profile;
use toyos_i219::ask::{Answer, Others, Reading};
use toyos_i219::crumbs::{self, Ending, Line, Step};
use toyos_i219::phy::{Outcome, PhyRefusal};

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

/// The same boot with netd's `--exit-with-phy-outcome` armed: netd ends right
/// after the bring-up with the PHY's outcome as its exit code, which the
/// kernel's `exit:` record carries off a machine whose console reaches nobody.
pub const PHY_CONFIG: &str = "tests/lanphycase";
pub const PHY_BOOT: &str = "lanphycase";

/// The same boot with netd's `--exit-with-mdio-ask` armed: netd resets the card,
/// puts one question to the MDIO arbitration, withdraws it, and ends with the
/// answer as its exit code. The card is never brought up on it.
pub const ASK_CONFIG: &str = "tests/lanaskcase";
pub const ASK_BOOT: &str = "lanaskcase";

/// [`PHY_BOOT`] with netd's `--exit-with-crumbs` armed instead: the same
/// bring-up ending with the same code, and a line on the log volume, flushed to
/// the stick, before every step of it.
pub const CRUMB_CONFIG: &str = "tests/lancrumbcase";
pub const CRUMB_BOOT: &str = "lancrumbcase";

/// The file that trail is left in, at the root of the log volume — netd's
/// `crumbs::PATH` under `/log`.
pub const CRUMBS_FILE: &str = "crumbs.txt";

/// netd, as the kernel's `exit:` record names it.
const NETD: &str = "netd";

/// The one job on that boot: it holds the machine up while the host pings it.
pub const JOBS: &[&str] = &["test_rs_lan_hold"];

/// The armed boot's judge: the kernel's own records, tied to the I219's
/// hand-over, say whether a message it raised reached a CPU — whatever the PHY
/// did about a link. Kernel records and not netd's: on this machine a userland
/// write reaches no channel the stick carries.
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

/// The same, with netd's `--exit-with-phy-outcome` armed.
const PHY_QEMU_CONFIG: &str = "tests/e1000phycase";

/// The same, with netd's `--exit-with-mdio-ask` armed.
const ASK_QEMU_CONFIG: &str = "tests/e1000askcase";

/// The same, with netd's `--exit-with-crumbs` armed.
const CRUMB_QEMU_CONFIG: &str = "tests/e1000crumbcase";

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

    for owed in [MAC, LINK_UP, READY] {
        if !text.contains(owed) {
            bad.push(format!("no {owed:?} record"));
        }
    }

    let mac = format!("{MAC}{}", cable.mac);
    if !text.contains(&mac) {
        bad.push(format!(
            "no {mac:?} record: the card this boot brought up is not the one that held {} \
             before it",
            cable.addr
        ));
    }

    match link_up_ms(text) {
        Ok(ms) => {
            eprintln!("  [lan] the link came up {ms} ms after the driver did");
            if let Err(why) = profile.judge(&format!("lan.{}.link_up_ms", back.label), ms) {
                bad.push(why.to_string());
            }
        }
        Err(why) => bad.push(why),
    }

    match lease_in(text) {
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
            match bootlog::host_second_inside_this_boot(text, cable.skew, LEASE, reply.at) {
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

/// The probe boot's judge: netd's exit code, decoded through the table the
/// driver crate owns. A refusal is a finding by its name, which is what the
/// shipping boot's silence cannot give — and where §4.5.2's interface was
/// already owned, that name is which of its three agents the last reading
/// before the deadline stood for.
pub fn probed_on_metal(probed: &metal::Readback) -> Result<(), String> {
    let code = probed.exit_code(NETD)?;
    match Outcome::from_exit_code(code) {
        Some(Outcome::BroughtUp) => {
            eprintln!("  [lan] netd exited {code} on {PHY_BOOT}: the PHY was brought up");
            Ok(())
        }
        Some(refused) => Err(format!(
            "netd exited {code} on {PHY_BOOT}: the bring-up refused the PHY with {refused:?}"
        )),
        None => Err(format!("netd exited {code} on {PHY_BOOT}, which is no outcome the probe encodes")),
    }
}

/// The ask boot's judge: the arbitration's answer. **A measurement and not a
/// verdict**: every reading the table encodes is what the boot was flashed to
/// learn, so the one red here is a code outside it.
pub fn asked_on_metal(asked: &metal::Readback) -> Result<(), String> {
    let code = asked.exit_code(NETD)?;
    match Reading::from_exit_code(code) {
        Some(reading) => {
            eprintln!("  [lan] netd exited {code} on {ASK_BOOT}: {reading}");
            Ok(())
        }
        None => Err(format!("netd exited {code} on {ASK_BOOT}, which is no reading the ask encodes")),
    }
}

/// The trail boot's judge. **Only a boot that came back is judged here**, and
/// a boot that came back owes a whole trail ending in the code its `exit:`
/// record carries; the boot the arm exists for is the one that does not come
/// back, whose file is read off the stick by hand and through [`trail_ending`].
pub fn trailed_on_metal(trailed: &metal::Readback) -> Result<(), String> {
    let code = trailed.exit_code(NETD)?;
    let text = trailed
        .log_volume_file(CRUMBS_FILE)?
        .ok_or_else(|| format!("{CRUMB_BOOT}'s log volume carries no {CRUMBS_FILE}"))?;
    let cost = whole_trail(&text, code)?;
    eprintln!("  [lan] {CRUMB_BOOT}: {cost}");
    Ok(())
}

/// What a crumb file says about how its boot ended, in one sentence — the
/// reading of a file copied off the stick of a machine that never came back.
pub fn trail_ending(text: &str) -> String {
    match Ending::of(text) {
        Ok(ending) => ending.to_string(),
        Err(why) => format!("the file is no trail: {why}"),
    }
}

/// What a trail cost the boot it was left on.
pub struct TrailCost {
    pub crumbs: usize,
    /// From the first crumb being handed to the device to the last one.
    pub window_ns: u64,
    /// Of that, what was spent between a crumb going to the device and coming
    /// back durable. The last crumb's is not in it: nothing after it says when
    /// it came back.
    pub writing_ns: u64,
    pub slowest_ns: u64,
}

impl std::fmt::Display for TrailCost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} crumbs over {} us, {} us of it writing them ({} us each on average, {} us the slowest)",
            self.crumbs,
            self.window_ns / 1_000,
            self.writing_ns / 1_000,
            self.writing_ns / 1_000 / (self.crumbs as u64 - 1).max(1),
            self.slowest_ns / 1_000,
        )
    }
}

/// A trail of a boot that came back: whole, ending in `code`, its clocks in
/// order, and between `dma-alloc` and `opened` exactly the bring-up the driver
/// crate says a trail owes, the PHY's own accesses apart.
pub fn whole_trail(text: &str, code: i32) -> Result<TrailCost, String> {
    let lines: Vec<Line> = crumbs::lines(text)
        .collect::<Result<_, _>>()
        .map_err(|why| format!("{CRUMBS_FILE} is no trail: {why}\n{text}"))?;
    match Ending::of(text) {
        Ok(Ending::Complete { code: said, .. }) if said == code => {}
        other => {
            return Err(format!(
                "netd exited {code} and its trail does not end in `exit {code}`: {}\n{text}",
                other.map_or_else(|why| why.to_string(), |ending| ending.to_string())
            ))
        }
    }
    let mut owed: Vec<String> =
        [Step::Start, Step::ClaimHeld, Step::Describe, Step::MapBar, Step::DmaAlloc]
            .iter()
            .map(|step| step.to_string())
            .collect();
    owed.extend(crumbs::BRING_UP.iter().map(|step| step.to_string()));
    owed.push(Step::Opened.to_string());
    owed.push(Step::Exit { code }.to_string());
    let left: Vec<String> = lines
        .iter()
        .map(|line| line.step.named().to_string())
        .filter(|named| !crumbs::is_the_phys(named))
        .collect();
    if left != owed {
        let at = left.iter().zip(&owed).position(|(l, o)| l != o).unwrap_or(left.len().min(owed.len()));
        return Err(format!(
            "the trail leaves the bring-up's order at crumb {at}: it says {:?} where {:?} is owed\n{text}",
            left.get(at),
            owed.get(at)
        ));
    }
    let mut cost = TrailCost { crumbs: lines.len(), window_ns: 0, writing_ns: 0, slowest_ns: 0 };
    for pair in lines.windows(2) {
        let (this, next) = (pair[0], pair[1]);
        if next.synced < this.at || next.at < next.synced {
            return Err(format!(
                "crumb {} went to the device at {} ns, and crumb {} says it came back at {} ns and was itself written at {} ns\n{text}",
                this.seq, this.at, next.seq, next.synced, next.at
            ));
        }
        let took = next.synced - this.at;
        cost.writing_ns += took;
        cost.slowest_ns = cost.slowest_ns.max(took);
    }
    cost.window_ns = lines[lines.len() - 1].at - lines[0].at;
    Ok(cost)
}

/// The trail, end to end, in front of QEMU's 82574 and on a USB stick like the
/// T14's: netd armed with the flag leaves `crumbs.txt` on the log volume, the
/// file is read back out of the image by the host's own FAT implementation, and
/// it is the whole bring-up in the driver crate's order, ending in the code the
/// kernel's `exit:` record carries. The outside checker has nothing to say
/// about the volume the file was left on.
///
/// The same config without the flag is booted beside it, so what the trail
/// costs the window netd holds the card for is a measured number.
pub fn lan_crumb_trail(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let case = super::compile::repo_root().join(CRUMB_QEMU_CONFIG);
    let image_path = super::lane::dir().join("lan-crumb-trail.img");
    let image = qemu::build_boot_image(&case, &[], &[], &[]);
    std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;
    let (start, len) = super::volumes::log_extent(&image, &image_path)?;

    let options = BootOptions {
        profile: qemu::Profile::E1000e,
        boot_image: Some(qemu::Staged::Written(image_path.clone())),
        ..Default::default()
    };
    let (code, console) = netd_exit_with(&case, options)?;
    let log = serial::Serial::named("the lan crumb boot", console.as_str());
    log.must_be_clean()?;
    if Outcome::from_exit_code(code) != Some(Outcome::NotThisRegisterMap) {
        return Err(format!(
            "netd exited {code} on the 82574 with a trail, and {PHY_QEMU_CONFIG} ends with NotThisRegisterMap's code"
        ));
    }

    let after = std::fs::read(&image_path).map_err(|e| format!("read the image back: {e}"))?;
    let volume = after.get(start..start + len).ok_or("the image shrank under the log partition")?;
    let text = super::volumes::read_files(volume, &[CRUMBS_FILE])?
        .pop()
        .flatten()
        .ok_or_else(|| format!("the log volume carries no {CRUMBS_FILE}"))?;
    let text = String::from_utf8(text).map_err(|e| format!("{CRUMBS_FILE}: {e}"))?;
    let cost = whole_trail(&text, code)?;
    let complaints = toyos_fat32_check::check(volume);
    if !complaints.is_empty() {
        return Err(format!(
            "the trail gave the checker something to say about the log volume:\n{}",
            toyos_fat32_check::describe(&complaints)
        ));
    }
    let _ = std::fs::remove_file(&image_path);

    // The same bring-up with no trail, for what the trail costs the window the
    // card is held for — both widths off the kernel's own two records.
    let with = held_ms(&console)?;
    let without = held_ms(&netd_exit_on_the_82574(PHY_QEMU_CONFIG)?.1)?;
    eprintln!("  [lan] {cost}");
    eprintln!(
        "  [lan] netd held the card for {with} ms with the trail and {without} ms without it"
    );
    eprintln!("  [lan] {}", trail_ending(&text));
    Ok(())
}

/// Milliseconds between the kernel handing the function over and netd's exit
/// record, which is the window a trail stretches.
fn held_ms(console: &str) -> Result<u64, String> {
    let at = |needle: &str| {
        console
            .lines()
            .find(|line| line.contains(needle))
            .and_then(bootlog::record_millis)
            .ok_or_else(|| format!("no timed `{needle}` record:\n{console}"))
    };
    let exited = format!("{}{NETD} pid=", bootlog::EXIT);
    Ok(at(&exited)?.saturating_sub(at("handed over on slot")?))
}

/// The probe's channel, end to end, on the one Intel part QEMU has: netd armed
/// with the flag exits with the outcome's code, the kernel records it, and the
/// record reads back through the driver crate's own table — which on the 82574
/// is `NotThisRegisterMap`, the refusal [`lan_dhcp_lease`] reads as text.
///
/// **No number is written here.** The block's codes are the driver crate's to
/// move, so this arm names the outcome and lets the one table say what it
/// exits with.
pub fn lan_phy_exit_code(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let (code, console) = netd_exit_on_the_82574(PHY_QEMU_CONFIG)?;
    let log = serial::Serial::named("the lan probe boot", console.as_str());
    match Outcome::from_exit_code(code) {
        Some(Outcome::NotThisRegisterMap) => {}
        other => {
            return Err(format!(
                "netd exited {code} on the 82574, which the table reads as {other:?} and not \
                 NotThisRegisterMap"
            ))
        }
    }
    log.must_say(&PhyRefusal::NotThisRegisterMap.to_string())?;
    log.must_be_clean()?;
    eprintln!(
        "  [lan] netd exited {code}, which the driver crate's table reads as NotThisRegisterMap"
    );
    Ok(())
}

/// Boot a config whose netd ends itself in front of QEMU's 82574, and hand back
/// the code the kernel's `exit:` record carries and the console it was read off.
fn netd_exit_on_the_82574(config: &str) -> Result<(i32, String), String> {
    let case = super::compile::repo_root().join(config);
    let options = BootOptions { profile: qemu::Profile::E1000e, ..Default::default() };
    netd_exit_with(&case, options)
}

fn netd_exit_with(case: &Path, options: BootOptions) -> Result<(i32, String), String> {
    if !qemu::profile_argv(&options).iter().any(|a| a.contains("e1000e")) {
        return Err("this test needs an Intel NIC and the profile has none".to_string());
    }
    let mut guest = QemuInstance::boot_with_options(case, &[], &[], options);
    let mut console = guest.boot_log().to_string();
    let exited = format!("{}{NETD} pid=", bootlog::EXIT);
    qemu::await_marker(&mut guest, &mut console, &exited, "netd to exit with its probe's code")?;
    drop(guest);
    let exit = metaldevices::exit_of(&console, NETD)
        .ok_or_else(|| format!("no readable `{exited}` record:\n{console}"))?;
    let code = i32::try_from(exit.code)
        .map_err(|_| format!("netd's exit record carries {}, which is no i32", exit.code))?;
    Ok((code, console))
}

/// The ask's channel, end to end, in front of a register file this repository
/// did not write: netd armed with the flag resets QEMU's 82574, registers
/// §4.5.2's software request, reads it back, withdraws it and exits with the
/// reading's code, and the record reads back through the driver crate's table.
///
/// Nothing in front of QEMU's 82574 shares its PHY, so the reading it owes is
/// the request read straight back with nobody else's bit beside it.
pub fn lan_mdio_ask_exit_code(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let (code, console) = netd_exit_on_the_82574(ASK_QEMU_CONFIG)?;
    let log = serial::Serial::named("the lan ask boot", console.as_str());
    let owed = Reading::Asked {
        before: Others::Nobody,
        answer: Answer::GrantedQuickly,
        after: Others::Nobody,
    };
    match Reading::from_exit_code(code) {
        Some(reading) if reading == owed => {}
        other => {
            return Err(format!(
                "netd exited {code} on the 82574, which the table reads as {other:?} and not \
                 {owed:?}"
            ))
        }
    }
    log.must_say(&owed.to_string())?;
    log.must_be_clean()?;
    eprintln!("  [lan] netd exited {code}: {owed}");
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

/// Where this process writes the frames one boot put on its wire.
fn wire_dump() -> std::path::PathBuf {
    let at = std::env::temp_dir().join(format!("toyos-lan-{}.pcap", std::process::id()));
    let _ = std::fs::remove_file(&at);
    at
}
