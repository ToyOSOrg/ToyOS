//! The cable: netd taking this machine's address from the network, and the T14
//! answering the development host on it.
//!
//! Every line read here is a line of a boot's log — the kernel's records, and
//! netd's own lines, which reach the stick under netd's name through its log
//! ring — or the one file netd leaves beside them, the lease probe's
//! report. The judge reads netd's lines by that name and no other program's.

use toyos_build::lan::{lease_in, link_up_ms, LINK_UP, MAC, READY};
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

/// The one job on that boot: it holds the machine up.
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

/// The PCI function that card is, as `/sys/bus/pci/devices` spells it: where
/// the metal loop reads the cable and the card's MAC before the flash.
pub const NIC: &str = "0000:00:1f.6";

/// The T14's judge: the claim, the card, and the lease in netd's own lines.
///
/// **The host learns the leased address from the boot**: netd answers for the
/// machine's name once it holds a lease, and the loop reads the log served at
/// the address the name answered with. The lease record is held to that
/// address here, so no lease but this boot's own is judged.
pub fn on_metal(back: &metal::Readback) -> Result<(), String> {
    let kernel = back.kernel();
    let text = kernel.text();
    let mut bad: Vec<String> = Vec::new();
    let wire_mac = back.wire_mac.as_ref().ok_or_else(|| {
        format!(
            "{}'s readback names no MAC: this boot was driven by a loop that was not told the \
             function its image claims",
            back.label
        )
    })?;
    let (heard, _) = back.talk()?;

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

    // Neither MAC is printed: a judge's lines are quoted in public.
    if !netd.contains(&format!("{MAC}{wire_mac}")) {
        bad.push(format!(
            "no {MAC:?} record names the MAC the operating system before this boot read on \
             {NIC}, which is `{}` in its `{}`: the card this boot brought up is not that one",
            toyos_build::metal::WIRE_MAC_KEY,
            toyos_build::metal::READBACK_BOOT
        ));
    }

    match link_up_ms(&netd) {
        Ok(ms) => eprintln!("  [lan] the link came up {ms} ms after the driver did"),
        Err(why) => bad.push(why),
    }

    match lease_in(&netd) {
        Ok(lease) => {
            // The resolvers are counted and not printed, for the same reason.
            eprintln!(
                "  [lan] leased {}/{} from {} in {} ms, gateway {}, {} resolver(s)",
                lease.address,
                lease.prefix,
                lease.server,
                lease.ms,
                lease.gateway,
                lease.dns.len()
            );
            if lease.address != heard.peer {
                bad.push(format!(
                    "this boot leased {} and answered for its name at {}: the address the host \
                     reached it at is not the one netd says it leased",
                    lease.address, heard.peer
                ));
            }
        }
        Err(why) => bad.push(why),
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
