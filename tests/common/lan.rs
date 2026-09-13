//! The cable: netd taking this machine's address from the network, and the T14
//! answering the development host on it.
//!
//! Every line read here is a record. On the T14 a userland `println!` reaches
//! `Backend::None`, so what crosses to the stick is the kernel's log — into
//! which netd's `say!` writes, being a `write` to a console object.

use std::net::Ipv4Addr;
use std::path::Path;

use toyos_build::bootlog;
use toyos_build::lan::{
    asked_under_its_own_name, lease_in, link_up_ms, Lease, HOSTNAME, LEASE, MAC, NO_LEASE, READY,
};
use toyos_build::metalprofile::Profile;

use super::metal;
use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

/// The boot config the T14 arm flashes, and the name every profile row for that
/// boot is under.
pub const CONFIG: &str = "tests/lancase";
pub const BOOT: &str = "lancase";

/// The one job on that boot: it holds the machine up while the host pings it.
pub const JOBS: &[&str] = &["test_rs_lan_hold"];

/// The config the QEMU arm boots — the Intel driver in front of the user-mode
/// backend, which is the same driver the T14 arm runs and the only DHCP server
/// this host can put in front of it.
const QEMU_CONFIG: &str = "tests/e1000case";

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

    // Not the link record: `link_up_ms` below already refuses its absence.
    if !text.contains(READY) {
        bad.push(format!("no {READY:?} record"));
    }

    // One fact, one finding: a boot that named no card at all is not a boot
    // that named a different one.
    let mac = format!("{MAC}{}", cable.mac);
    if !text.contains(&mac) {
        bad.push(match text.lines().find(|l| l.contains(MAC)) {
            Some(line) => format!(
                "{}: the card this boot brought up is not the one that held {} before it",
                line.trim(),
                cable.addr
            ),
            None => format!("no {MAC:?} record"),
        });
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
                "  [lan] leased {}/{} from {} in {} ms, gateway {:?}, dns {:?}",
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
            if let Err(why) = profile.judge(&format!("boot.{}.ping_secs", back.label), reply.secs) {
                bad.push(why.to_string());
            }
            if let Err(why) =
                bootlog::host_second_inside_this_boot(text, cable.skew, LEASE, reply.at)
            {
                bad.push(why);
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
