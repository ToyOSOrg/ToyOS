//! The cable: netd taking this machine's address from the network, and the T14
//! answering the development host on it.
//!
//! Every line read here is a line of a boot's log — the kernel's records, and
//! netd's own lines, which reach the stick under netd's name through its log
//! ring — or the one file netd leaves beside them, the lease probe's
//! report. The judge reads netd's lines by that name and no other program's.

use toyos_build::bootlog;
use toyos_build::lan::{
    lease_in, link_up_ms, LEASE, LINK_UP, MAC,
    READY,
};
use toyos_i219::lease::{self, Event, Verdict};

use super::metal;

/// The boot with netd's `--exit-with-lease` armed: netd brings the card up and
/// serves, leaves [`LEASE_FILE`] on the log volume one durable line at a time,
/// and ends with the lease's verdict as its exit code, which the kernel's
/// `exit:` record carries off a machine whose console reaches nobody.
pub const LEASE_CONFIG: &str = "tests/lanleasecase";
pub const LEASE_BOOT: &str = "lanleasecase";

/// The file that report is left in, at the root of the log volume — netd's
/// `report::PATH` under `/log`.
pub const LEASE_FILE: &str = "lease.txt";

/// netd, as the kernel's `exit:` record names it.
const NETD: &str = "netd";

/// The one job on that boot: it holds the machine up while the host pings it.
pub const JOBS: &[&str] = &["test_rs_lan_hold"];

/// The boot the host talks to over its own cable: the log it serves, sshd, and
/// `reboot` as the way the machine is handed back.
pub const TALK_CONFIG: &str = "tests/lantalkcase";
pub const TALK_BOOT: &str = "lantalkcase";

/// Its one job holds the machine until the runner's bound is near, as the
/// fallback for a host that never tells it to reboot.
const TALK_HOLD: &str = "test_rs_lan_talk_hold";
pub const TALK_JOBS: &[&str] = &[TALK_HOLD];

/// The kernel's own records, tied to the I219's hand-over, say a message it
/// raised reached a CPU — whatever the PHY did about a link.
pub fn delivered_on_metal(back: &metal::Readback) -> Result<(), String> {
    let got = toyos_build::lan::delivered(back.kernel().text())?;
    eprintln!("  [lan] {}", got.handed.trim());
    eprintln!("  [lan] {}", got.took.trim());
    Ok(())
}

/// The card the T14 arm claims, as the kernel and the manifest spell it.
const ID: &str = "8086:15fc";

/// The PCI function that card is, as `/sys/bus/pci/devices` spells it: the
/// cable the metal loop reaches this boot over while it runs.
pub const NIC: &str = "0000:00:1f.6";

/// The T14's judge: the claim, the card, the lease, and the host's own ping.
pub fn on_metal(back: &metal::Readback) -> Result<(), String> {
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
                 {ID}, so `tests/lantalkcase` was flashed onto a machine that has no such card"
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
        Ok(ms) => eprintln!("  [lan] the link came up {ms} ms after the driver did"),
        Err(why) => bad.push(why),
    }

    match lease_in(&netd) {
        Ok(lease) => {
            eprintln!(
                "  [lan] leased {}/{} from {} in {} ms, gateway {}, dns {:?}",
                lease.address, lease.prefix, lease.server, lease.ms, lease.gateway, lease.dns
            );
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
            bad.extend(
                bootlog::host_second_inside_this_boot(log.text(), cable.skew, LEASE, reply.at).err(),
            );
        }
        None => bad.push(format!(
            "nothing answered a ping at {} while this machine was between its two operating \
             systems",
            cable.addr
        )),
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

/// The T14 talked to over its own cable, judged from the Mac's side: the log
/// the machine served, asked for by its name while it booted, is the stick's
/// own log from its first line in its own order, the address the name answered
/// with answered a ping and ran the command, and `reboot` handed the machine
/// back — before its hold would have.
pub fn talked_on_metal(back: &metal::Readback) -> Result<(), String> {
    let (heard, stream) = back.talk()?;
    let mut bad: Vec<String> = Vec::new();
    match toyos_build::metaltalk::judge(&heard, &stream) {
        Ok(said) => said.iter().for_each(|line| eprintln!("  [talk] {line}")),
        Err(found) => bad.extend(found),
    }
    // **The stick is the oracle the wire is compared with**: `logd` writes a
    // round to `/log` and then hands it to every reader from the boot's first
    // line, so what arrived is the file's own first lines in the file's own
    // order — and the file came back over a different path.
    let file: Vec<String> = back.log().text().split_inclusive('\n').map(str::to_string).collect();
    match super::logstream::is_prefix_of(&stream, &file) {
        Ok(()) => eprintln!(
            "  [talk] the {} served line(s) are the stick's own, from its first, in its order \
             ({} in the file)",
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
