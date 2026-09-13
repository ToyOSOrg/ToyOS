//! The cable: netd taking this machine's address from the network, and the T14
//! answering the development host on it.
//!
//! **The two arms read different channels.** A guest's console is a serial port
//! this process holds the other end of, so the QEMU arm reads netd's own
//! records. The T14 has no serial port: a userland write ends at
//! `Backend::None`, no `netd:` line has ever reached the stick, and the T14 arm
//! reads what the kernel records — the hand-over, the interrupt census and the
//! `exit:` code the job that asked netd left — beside what the loop measured
//! off the cable itself.

use std::net::Ipv4Addr;
use std::path::Path;

use toyos_build::bootlog;
use toyos_build::lan::{
    asked_under_its_own_name, job_said, kernel_function, lease_in, link_up_ms, Lease, HOSTNAME,
    LEASE, MAC, NIC, NO_LEASE, READY,
};
use toyos_build::metalprofile::Profile;

use super::irqcensus::Census;
use super::metal;
use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

/// The boot config the T14 arm flashes, and the name every profile row for that
/// boot is under.
pub const CONFIG: &str = "tests/lancase";
pub const BOOT: &str = "lancase";

/// That boot's job list, in order: one holds the machine up while the host
/// pings it, and the second asks netd what network it is on — after the ping
/// window, so a machine that answers is a machine still holding the cable.
pub const JOBS: &[&str] = &["test_rs_lan_hold", "test_rs_lan_state"];

/// The config the QEMU arm boots — the Intel driver in front of the user-mode
/// backend, which is the same driver the T14 arm runs and the only DHCP server
/// this host can put in front of it.
const QEMU_CONFIG: &str = "tests/e1000case";

/// The card the T14 arm claims, as the kernel and the manifest spell it.
const ID: &str = "8086:15fc";

/// The census source a NIC a *process* drives raises its interrupts under.
const USERDEV: &str = "userdev";

/// The binary behind [`JOBS`]`[1]`, as the build stages it: the runner spawns
/// it under the `test_rs_` prefix and `rust_bins` carries it under its own.
const ASKER: &str = "lan_state";

/// The T14's judge: the hand-over, what netd answered the job that asked it,
/// the interrupts that answer cost, and the host's own ping.
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

    // The function the loop reached this boot over is named in the needle, so
    // the card the host read its MAC off and the card the kernel gave netd are
    // one record rather than two checks. A boot with no hand-over line carries
    // the kernel's refusal instead, and quoting that is the whole diagnosis.
    let handed = format!("PCI {} [{ID}] handed over on slot", kernel_function(NIC));
    let handover = text.lines().find(|l| l.contains(&handed));
    match handover {
        Some(line) => eprintln!("  [lan] {}", line.trim()),
        None => bad.push(match text.lines().find(|l| l.contains("NOT HANDED OVER")) {
            Some(line) => format!("the kernel refused this function: {}", line.trim()),
            None => format!(
                "no `{handed}` record and no refusal either: nothing on this machine claimed \
                 {ID}, so `tests/lancase` was flashed onto a machine that has no such card"
            ),
        }),
    }

    // What netd held, folded through the one word a machine with no console
    // has. `Ok` is the address that answered the ping on the card the host read
    // off the wire, so the address check the log used to carry is inside it.
    let leased = match back.exit_code(JOBS[1]).and_then(|code| {
        job_said(code, cable.addr, &cable.mac).map(|()| code)
    }) {
        Ok(code) => {
            eprintln!("  [lan] netd held {} on {} ({code})", cable.addr, cable.mac);
            true
        }
        Err(why) => {
            bad.push(why);
            false
        }
    };

    // **A lease with no interrupt is a contradiction.** DHCP is a round trip on
    // the wire and this kernel delivers a user-driven NIC's vector to the
    // process that claimed it, so a boot that leased and counted none did not
    // lease — it read somebody else's answer, or this census is not of this
    // card.
    if leased {
        match userdev_raised(text) {
            Ok(0) => bad.push(format!(
                "netd answered with a lease and every `irq:` line of this boot reads \
                 {USERDEV}=0: the card raised no interrupt, so nothing it received reached the \
                 driver"
            )),
            Ok(raised) => eprintln!("  [lan] the card raised {raised} interrupt(s) into netd"),
            Err(why) => bad.push(why),
        }
    }

    match cable.reply {
        Some(reply) => {
            eprintln!(
                "  [lan] {} answered the host's ping {} s into the window",
                cable.addr, reply.secs
            );
            if let Err(why) = profile.judge(&format!("boot.{}.ping_secs", back.label), reply.secs) {
                bad.push(why.to_string());
            }
            // The lower edge is the hand-over: nothing on this machine could
            // answer on that function before the kernel gave it to a driver,
            // and it is the earliest record of this boot that is true of. One
            // fact, one finding — a boot with no hand-over record has already
            // said so above, and a bracket it cannot compute is that absence
            // again rather than something else about the reply.
            if handover.is_some() {
                if let Err(why) =
                    bootlog::host_second_inside_this_boot(text, cable.skew, &handed, reply.at)
                {
                    bad.push(why);
                }
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

/// Interrupts this boot delivered to a process that drives a device, summed
/// over the machine.
///
/// The counters are cumulative, so a CPU's last census is its whole boot; a
/// boot that printed none is refused rather than summed to zero, which would
/// read as the card being silent when it is the kernel that said nothing.
fn userdev_raised(text: &str) -> Result<u64, String> {
    let mut last: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    for line in text.lines() {
        match Census::parse(line) {
            None => continue,
            Some(Ok(census)) => {
                last.insert(census.cpu, census.source(USERDEV));
            }
            Some(Err(why)) => return Err(format!("{why}\nline: {line}")),
        }
    }
    if last.is_empty() {
        return Err("no `irq: cpu` census in this boot's log, so nothing here says whether the \
                    card raised an interrupt"
            .to_string());
    }
    Ok(last.values().sum())
}

/// The QEMU arm: the client, against a DHCP server this repository did not
/// write.
///
/// Every field of the lease is checked, because a client that dropped the router
/// option or read the mask off the wrong one would otherwise pass where the
/// answers happen to agree; and the readiness line is checked to come *after*
/// the lease, because every other arm waits for it and then connects.
///
/// **It is also the only arm that runs the T14's own judge end to end.** The
/// job that asks netd, netd's answer and the fold the exit code carries are
/// what the metal arm has instead of a log, and here the same three are read
/// against records — netd's `MAC` line and its lease line — that the T14 does
/// not have.
pub fn lan_dhcp_lease(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let case = super::compile::repo_root().join(QEMU_CONFIG);
    let dump = std::env::temp_dir().join(format!("toyos-lan-{}.pcap", std::process::id()));
    let _ = std::fs::remove_file(&dump);
    let options = BootOptions {
        profile: qemu::Profile::E1000e,
        wire_dump: Some(dump.clone()),
        ..Default::default()
    };
    if !qemu::profile_argv(&options).iter().any(|a| a.contains("e1000e")) {
        return Err("this test needs an Intel NIC and the profile has none".to_string());
    }
    let asker: Vec<(String, Vec<u8>)> =
        rust_bins.iter().filter(|(name, _)| name == ASKER).cloned().collect();
    if asker.is_empty() {
        return Err(format!("{ASKER} was not built"));
    }
    let mut guest = QemuInstance::boot_with_options(&case, &[], &asker, options);
    let mut console = guest.boot_log().to_string();
    qemu::await_marker(&mut guest, &mut console, READY, "netd to take an address")?;
    let asked = guest.run_test(JOBS[1], std::time::Duration::from_secs(30));
    console.push_str(&asked.before);
    console.push_str(&asked.serial);
    console.push_str(&guest.drain_serial(std::time::Duration::from_millis(500)));
    // QEMU owns the pcap while it runs, and every refusal below is a return:
    // the frames are taken once the machine is gone and the file removed here.
    drop(guest);
    let frames = std::fs::read(&dump).map_err(|e| format!("{}: {e}", dump.display()))?;
    let _ = std::fs::remove_file(&dump);
    let log = serial::Serial::named("the lan boot", console.as_str());

    let lease = lease_in(log.text())?;
    // QEMU's user-mode backend's own defaults, not this repository's: the oracle.
    let want = Lease {
        address: Ipv4Addr::new(10, 0, 2, 15),
        prefix: 24,
        server: Ipv4Addr::new(10, 0, 2, 2),
        gateway: Some(Ipv4Addr::new(10, 0, 2, 2)),
        dns: vec![Ipv4Addr::new(10, 0, 2, 3)],
        ms: lease.ms,
    };
    if lease != want {
        return Err(format!(
            "the client read this lease as {lease:?} and the backend serves {want:?}"
        ));
    }
    // The order, and not merely the presence of both.
    log.must_say_after(LEASE, READY)?;
    let ms = link_up_ms(log.text())?;
    eprintln!(
        "  [lan] the emulated link came up in {ms} ms and the lease landed {} ms after netd \
         started",
        lease.ms
    );
    // What the job that asks netd exits with, against the two records netd
    // wrote about the same two facts. **Nothing in the guest computes both
    // sides**: netd answered the job out of its interface and wrote these lines
    // out of the driver, and the fold is taken here.
    let announced = log
        .text()
        .lines()
        .find_map(|l| l.split(MAC).nth(1)?.split_whitespace().next())
        .ok_or_else(|| format!("no {MAC:?} record: netd named no card"))?;
    let code = asked
        .exit_code
        .ok_or_else(|| format!("{ASKER} left no exit code: {:?}\n{}", asked.error, asked.stdout))?;
    if let Err(why) = job_said(code, lease.address, announced) {
        return Err(format!("{why}\n{}", asked.stdout));
    }
    eprintln!("  [lan] netd answered {ASKER} with {announced} and {}", lease.address);
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
