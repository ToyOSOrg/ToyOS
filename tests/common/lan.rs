//! The cable: netd taking this machine's address from the network, and the T14
//! answering the development host on it.
//!
//! **The two arms answer different questions.** Under QEMU the DHCP server is
//! the user-mode backend's own, an implementation of RFC 2131 this repository
//! did not write, and what it certifies is the client: the lease it hands out
//! is known — `10.0.2.15/24`, gateway and server `10.0.2.2`, resolver
//! `10.0.2.3` — so a client that mis-parses any field is caught by name. On the
//! T14 the server is the bench's router, the lease is whatever it has for this
//! MAC, and what is certified is the whole path: the kernel handing netd the
//! I219's function, the driver bringing its link up, the lease, and the
//! development host's own `ping` being answered at the leased address while the
//! machine is running nothing else.
//!
//! Every line read here is a record. On the T14 a userland `println!` reaches
//! `Backend::None`, so what crosses to the stick is the kernel's log — into
//! which netd's `say!` writes, being a `write` to a console object.

use std::path::Path;

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

/// What QEMU's user-mode backend leases, and what it says about the network it
/// leases on. Its own defaults, not this repository's: they are the oracle.
const SLIRP_ADDRESS: &str = "10.0.2.15";
const SLIRP_PREFIX: u8 = 24;
const SLIRP_ROUTER: &str = "10.0.2.2";
const SLIRP_DNS: &str = "10.0.2.3";

/// The card the T14 arm claims, as the kernel and the manifest spell it.
const ID: &str = "8086:15fc";

/// The records this pair of arms is written against, spelled once.
///
/// They are netd's own `say!` lines, and netd is another crate: what holds the
/// two spellings together is that a boot missing any of these fails here by
/// name rather than passing quietly.
const MAC: &str = "netd: MAC ";
const LEASE: &str = "netd: DHCP: lease ";
const LINK_UP: &str = "netd: I219: link up at ";
const READY: &str = "netd: ready, at most ";

/// One lease, as the record carries it.
#[derive(Debug, PartialEq, Eq)]
pub struct Lease {
    pub address: String,
    pub prefix: u8,
    pub server: String,
    pub gateway: String,
    pub dns: Vec<String>,
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
    let cidr = after(LEASE, " from ")?;
    let (address, prefix) = cidr.split_once('/').ok_or_else(|| unreadable("an address/prefix"))?;
    let dns = after(", dns [", "]")?;
    Ok(Lease {
        address: address.to_string(),
        prefix: prefix.parse().map_err(|_| unreadable("a prefix length"))?,
        server: after(" from ", ",")?,
        gateway: after(", gateway ", ",")?,
        dns: dns.split_whitespace().map(str::to_string).collect(),
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

/// The T14's judge: the claim, the card, the lease, and the host's own ping.
///
/// **The ping and the lease are held to each other.** The address the host
/// pinged is the one this machine's name resolved to before the boot; the
/// address the boot leased is in its own record; a run where those differ is a
/// ping answered by something that is not this boot.
pub fn on_metal(back: &metal::Readback) -> Result<(), String> {
    let profile = Profile::load(&super::compile::repo_root()).map_err(|why| why.to_string())?;
    let kernel = back.kernel();
    let text = kernel.text();
    let mut bad: Vec<String> = Vec::new();

    // The kernel's own account of the hand-over, which is where an interrupt
    // mechanism the substrate cannot arm is refused by name. A boot with no
    // hand-over line carries the refusal instead, and quoting it is the whole
    // diagnosis.
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
            if lease.address != back.ping_addr {
                bad.push(format!(
                    "this boot leased {} and the host pinged {}, so whatever answered was not \
                     this boot",
                    lease.address, back.ping_addr
                ));
            }
        }
        Err(why) => bad.push(why),
    }

    match back.ping_secs {
        Some(secs) => {
            eprintln!(
                "  [lan] {} answered the host's ping {secs} s into the window",
                back.ping_addr
            );
            // Judged here as well as by the boot loop, because this is the arm
            // that *claims* the number: a ceiling nobody wrote is what tells
            // Ubuntu's reply on its way back up from this boot's, and the
            // profile refuses an unpriced name rather than passing it.
            if let Err(why) = profile.judge(&format!("boot.{}.ping_secs", back.label), secs) {
                bad.push(why.to_string());
            }
        }
        None => bad.push(format!(
            "nothing answered a ping at {} while this machine was between its two operating \
             systems",
            back.ping_addr
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
/// Every field of the lease is checked against what the user-mode backend
/// serves, because a client that dropped the router option or read the mask off
/// the wrong option would otherwise pass on a machine where the answers happen
/// to agree. And the readiness line is checked to come *after* the lease: every
/// other arm in this suite waits for that line and then connects, so a netd that
/// announced itself before it had an address would hand those arms a stack with
/// none.
pub fn lan_dhcp_lease(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let case = super::compile::repo_root().join(QEMU_CONFIG);
    let options = BootOptions { profile: qemu::Profile::E1000e, ..Default::default() };
    if !qemu::profile_argv(&options).iter().any(|a| a.contains("e1000e")) {
        return Err("this test needs an Intel NIC and the profile has none".to_string());
    }
    let mut guest = QemuInstance::boot_with_options(&case, &[], &[], options);
    let mut console = guest.boot_log().to_string();
    qemu::await_marker(&mut guest, &mut console, READY, "netd to take an address")?;
    console.push_str(&guest.drain_serial(std::time::Duration::from_millis(500)));
    let log = serial::Serial::named("the lan boot", console.as_str());

    let lease = lease_in(log.text())?;
    let want = Lease {
        address: SLIRP_ADDRESS.to_string(),
        prefix: SLIRP_PREFIX,
        server: SLIRP_ROUTER.to_string(),
        gateway: SLIRP_ROUTER.to_string(),
        dns: vec![SLIRP_DNS.to_string()],
        ms: lease.ms,
    };
    if lease != want {
        return Err(format!(
            "the client read this lease as {lease:?} and the backend serves {want:?}"
        ));
    }
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
    Ok(())
}
