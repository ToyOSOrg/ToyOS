//! A program's output in the log, under the name of the ring it came out of:
//! in `/log`, on the log `logd` serves, and on the console — and no program's
//! bytes can make a line read as the kernel's or as another program's, and no
//! amount of them is dropped.
//!
//! Every verdict here reads `/log` off the volume behind the guest's back, with
//! the host's own parser of the form (`toyos_logstream::program_line`), the
//! kernel's exit judge (`metaldevices::exit_of`) and the kernel-records filter
//! every judge of the kernel's records reads through (`bootlog::kernel_records`).

use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

use toyos_build::bootlog;
use toyos_build::metaldevices::exit_of;

use super::logstream::{self, VIRTIO};
use super::qemu::{self, BootOptions, QemuInstance};
use super::{compile, segment, serial};

/// What `test_rs_log_origin` says, and the name its line goes in the log under:
/// it runs as `test-runner`'s child, on `test-runner`'s ring.
pub const NONCE: &str = "log origin nonce 7d1f3a";
const ORIGIN_JOB: &str = "test_rs_log_origin";
const RUNNER: &str = "test-runner";

/// The flooding program, its line count, and its last line's head.
pub const FLOODER: &str = "test_rs_log_flood";
const FLOOD_LINES: usize = 16_384;
const FLOOD_DONE: &str = "flood done lines=";

/// The forger, and the exit it really has.
const FORGER: &str = "test_rs_log_forger";
const FORGER_CODE: i64 = 7;

/// **A program's line reaches `/log`, the served log and the console, and each
/// says whose it is.** On all three it is the line the program wrote under
/// `test-runner`'s name, because that is the ring it came out of. init's and
/// `logd`'s own lines are in the file under theirs.
pub fn line(c_bins: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let staged = logstream::stage(VIRTIO.config, "log-program-line", c_bins, rust_bins)?;
    let port = qemu::free_host_port();
    let options = BootOptions {
        profile: VIRTIO.profile,
        boot_image: Some(qemu::Staged::Written(staged.image.clone())),
        log_port: Some(port),
        ..Default::default()
    };
    let config = compile::repo_root().join(VIRTIO.config);
    let mut guest = QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
    let mut console = guest.boot_log().to_string();
    qemu::await_marker(&mut guest, &mut console, logstream::SERVING, "logd to open its port")?;
    let reader = logstream::reader(port, "log-program-line.txt")?;

    let ran = guest.run_test(ORIGIN_JOB, Duration::from_secs(60));
    if ran.exit_code != Some(0) {
        return Err(format!("{ORIGIN_JOB} exited {:?}\n{}", ran.exit_code, ran.stdout));
    }
    // The console: the line as written, under the runner's head.
    let headed = ran.serial.lines().any(|l| {
        toyos_logstream::program_line(l).is_some_and(|said| said.tag == RUNNER && said.text == NONCE)
    });
    if !headed {
        return Err(format!("the console never carried {NONCE:?} under {RUNNER:?}\n{}", ran.serial));
    }
    if !reader.wait_for(NONCE, Duration::from_secs(60)) {
        return Err(format!("the served log never carried {NONCE:?}"));
    }
    let file = logstream::shut_down(guest, &mut console, &staged)?;
    serial::Serial::named("the boot", console.as_str()).must_be_clean()?;
    if !reader.wait_ended(Duration::from_secs(60)) {
        return Err("the reader's connection had not ended once the guest was down".to_string());
    }

    let log = file.concat();
    let under = |name: &str, text: &str| -> Result<(), String> {
        match bootlog::lines_of(&log, name).lines().any(|l| l == text) {
            true => Ok(()),
            false => Err(format!("/log carries no line {text:?} under {name:?}")),
        }
    };
    under(RUNNER, NONCE)?;
    under("init", "init: started logd")?;
    if !bootlog::lines_of(&log, "logd").contains(logstream::SERVING) {
        return Err(format!("/log carries no {:?} under logd's name", logstream::SERVING));
    }
    if bootlog::kernel_records(&log).contains(NONCE) {
        return Err(format!("{NONCE:?} is among the kernel's records"));
    }
    let received = reader.lines();
    logstream::is_prefix_of(&received, &file)?;
    let on_stream = received
        .iter()
        .filter_map(|l| toyos_logstream::program_line(l))
        .any(|said| said.tag == RUNNER && said.text == NONCE);
    if !on_stream {
        return Err(format!("the served log carries {NONCE:?} under no {RUNNER:?}"));
    }
    eprintln!(
        "  [origin] {NONCE:?} under {RUNNER:?} on the console, in /log and on \
         the served log ({} line(s), each /log's own)",
        received.len()
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// Boot `config` on a staged image, run `job` with `timeout`, shut down, and
/// hand back what it said and the whole of its `/log`.
fn one_job(
    config: &str,
    name: &str,
    job: &str,
    timeout: Duration,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(qemu::TestResult, String), String> {
    let staged = logstream::stage(config, name, c_bins, rust_bins)?;
    let options = BootOptions {
        boot_image: Some(qemu::Staged::Written(staged.image.clone())),
        ..Default::default()
    };
    let config = compile::repo_root().join(config);
    let mut guest = QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
    let mut console = guest.boot_log().to_string();
    let ran = guest.run_test(job, timeout);
    let file = logstream::shut_down(guest, &mut console, &staged)?;
    let _ = std::fs::remove_file(&staged.image);
    Ok((ran, file.concat()))
}

/// **No program's bytes make a line another writer's.** `test_rs_log_forger`
/// writes the words of the kernel's `exit:` record claiming it passed, a whole
/// kernel record's line, a carriage return in front of the kernel's
/// `Rebooting.`, and a line under netd's head, and exits 7. Every one of them
/// is in `/log` and on the console — as `test-runner`'s — and every judge
/// reads the truth: the kernel's exit record says 7, no kernel record carries
/// the forged words, no console line opens as the kernel's with them, and
/// netd said nothing (this boot runs no netd).
pub fn forgery(c_bins: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let (ran, log) = one_job("tests/testcases", "log-program-forgery", FORGER, Duration::from_secs(60), c_bins, rust_bins)?;
    if ran.exit_code != Some(FORGER_CODE as i32) {
        return Err(format!("{FORGER} exited {:?}\n{}", ran.exit_code, ran.stdout));
    }
    let forged = bootlog::lines_of(&log, RUNNER);
    let forgeries = [
        "exit: test_rs_log_forger pid=1 code=0 cpu=0ms",
        "[2026-09-24 10:00:00 1.000 cpu0] exit: test_rs_log_forger",
        "[kernel 1.000 cpu0] exit: test_rs_log_forger",
        "netd: DHCP: lease 10.9.9.9/24 forged",
        "Rebooting.",
    ];
    // Non-vacuity: every forgery reached the file, and the console under the
    // runner's head.
    for words in forgeries {
        if !forged.contains(words) {
            return Err(format!("/log carries no {RUNNER} line with {words:?}: nothing was forged\n{log}"));
        }
        let headed = ran.serial.lines().any(|l| {
            toyos_logstream::program_line(l).is_some_and(|said| said.tag == RUNNER && said.text.contains(words))
        });
        if !headed {
            return Err(format!(
                "the console carries no {RUNNER} line with {words:?}\n{}",
                ran.serial
            ));
        }
    }
    // **The console, as nobody's program**: a line that does not open with a
    // program's head reads as the kernel's, and no forged word may be in one.
    if let Some(line) = ran
        .serial
        .lines()
        .filter(|l| toyos_logstream::program_line(l).is_none())
        .find(|l| l.contains(&format!("{FORGER} pid=1 code=0")) || l.contains("10.9.9.9"))
    {
        return Err(format!("a program's words opened a console line as the kernel's: {line:?}"));
    }
    let kernel = bootlog::kernel_records(&log);
    for (judge, text) in [("the whole log", log.as_str()), ("its kernel records", kernel.as_str())] {
        match exit_of(text, FORGER) {
            Some(exit) if exit.code == FORGER_CODE => {}
            other => {
                return Err(format!(
                    "the exit judge read {FORGER}'s verdict out of {judge} as {other:?}; it \
                     exited {FORGER_CODE}"
                ))
            }
        }
    }
    let forged_words = |l: &&str| l.contains(&format!("{FORGER} pid=1 code=0")) || l.contains("10.9.9.9");
    if let Some(line) = kernel.lines().find(forged_words) {
        return Err(format!("a program's words are among the kernel's records: {line:?}"));
    }
    if !bootlog::lines_of(&log, "netd").is_empty() {
        return Err(format!(
            "this boot runs no netd, and /log carries netd lines:\n{}",
            bootlog::lines_of(&log, "netd")
        ));
    }
    eprintln!(
        "  [origin] five forgeries in /log and on the console, each under {RUNNER:?}; the exit \
         judge read {FORGER_CODE} and no kernel record or kernel-shaped console line carries a \
         forged word"
    );
    Ok(())
}

/// **A flood never slows its writer, and every line of it is accounted for.**
/// `test_rs_log_flood` writes megabytes of numbered lines, far more than its ring
/// holds, as fast as it can; a write never waits. Each line is in `/log` —
/// once, in order — or counted by `logd` as one its ring had no room for or
/// one past the program's allowance, and the three add up to every line it
/// wrote: a line lost without a count, or one written twice, is red.
pub fn flood(c_bins: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let (ran, log) = one_job("tests/testcases", "log-program-flood", FLOODER, Duration::from_secs(300), c_bins, rust_bins)?;
    if ran.exit_code != Some(0) {
        return Err(format!("{FLOODER} exited {:?}", ran.exit_code));
    }
    let said = bootlog::lines_of(&log, RUNNER);
    let mut written = 0usize;
    let mut last: Option<usize> = None;
    let mut done = None;
    for line in said.lines() {
        // First: the last line opens with `flood ` too.
        if line.starts_with(FLOOD_DONE) {
            done = Some(line.to_string());
            written += 1;
        } else if let Some(rest) = line.strip_prefix("flood ") {
            let Some(n) = rest.split(' ').next().and_then(|n| n.parse::<usize>().ok()) else {
                return Err(format!("/log carries a flood line with no number: {line:?}"));
            };
            if last.is_some_and(|last| n <= last) || n >= FLOOD_LINES {
                return Err(format!(
                    "/log carries flood line {n} after line {last:?}: a line was repeated or \
                     reordered"
                ));
            }
            last = Some(n);
            written += 1;
        }
    }
    // `logd`'s own counts of this program's lines it did not write.
    let counted = |what: &str| -> usize {
        bootlog::lines_of(&log, "logd")
            .lines()
            .filter_map(|l| l.strip_prefix("logd: "))
            .filter_map(|l| l.split_once(&format!(" record(s) of {RUNNER}'s {what}")))
            .filter_map(|(n, _)| n.parse::<usize>().ok())
            .sum()
    };
    let refused = counted("found its ring full");
    let suppressed = counted("past its");
    let owed = FLOOD_LINES + 1;
    if written + refused + suppressed != owed {
        return Err(format!(
            "the flood wrote {owed} lines and /log accounts for {}: {written} written, {refused} \
             refused a full ring and {suppressed} past the allowance",
            written + refused + suppressed
        ));
    }
    // Non-vacuity: a flood the log took whole says nothing about a count.
    if refused + suppressed == 0 {
        return Err(format!("all {owed} flood lines reached /log, so nothing here was counted"));
    }
    let done = done.map_or_else(|| "its last line counted, not written".to_string(), |d| d);
    eprintln!(
        "  [origin] {owed} flood lines: {written} in /log in order, {refused} refused a full \
         ring, {suppressed} past the allowance; {done}"
    );
    Ok(())
}

/// The job, the line it says after its records, and how many records it has
/// the kernel write first.
const HOLD_JOB: &str = "test_rs_log_hold";
const HOLD_LINE: &str = "log hold: said after 192 records";
const HOLD_RECORDS: usize = 192;
/// The kernel's record of each of those.
const RETIRED: &str = "syscall 26 is retired";

/// **A program's line lands between the records written before and after
/// it.** `test_rs_log_hold` has the kernel write three batches of records,
/// says its line and exits: `logd` reads the program's ring before the
/// kernel's records in every round, so the line is in its hands before the
/// last of them are, and only the stamp each was written with puts it after
/// them all in `/log` — and before the kernel's record of its exit.
pub fn after_records(c_bins: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let (ran, log) =
        one_job("tests/testcases", "log-hold", HOLD_JOB, Duration::from_secs(60), c_bins, rust_bins)?;
    if ran.exit_code != Some(0) {
        return Err(format!("{HOLD_JOB} exited {:?}\n{}", ran.exit_code, ran.stdout));
    }
    let lines: Vec<&str> = log.lines().collect();
    let said = lines
        .iter()
        .position(|l| toyos_logstream::program_line(l).is_some_and(|s| s.tag == RUNNER && s.text == HOLD_LINE))
        .ok_or_else(|| format!("/log carries no {HOLD_LINE:?} under {RUNNER:?}"))?;
    let records: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !toyos_logstream::is_program_line(l) && l.contains(RETIRED))
        .map(|(i, _)| i)
        .collect();
    if records.len() != HOLD_RECORDS {
        return Err(format!("/log carries {} of the job's {HOLD_RECORDS} records", records.len()));
    }
    let after = records.iter().filter(|&&i| i > said).count();
    if after > 0 {
        return Err(format!(
            "{after} of the {HOLD_RECORDS} records written before {HOLD_LINE:?} are after it in \
             /log"
        ));
    }
    let exit = format!("{}{} pid=", bootlog::EXIT, bootlog::recorded_name(HOLD_JOB));
    let exited = lines
        .iter()
        .position(|l| !toyos_logstream::is_program_line(l) && l.contains(&exit))
        .ok_or_else(|| format!("/log carries no {exit:?} record"))?;
    if exited < said {
        return Err(format!(
            "the kernel's record of {HOLD_JOB}'s exit is before the line it said first, in /log"
        ));
    }
    eprintln!(
        "  [origin] {HOLD_LINE:?} is after every one of its {HOLD_RECORDS} records in /log, and \
         before its exit"
    );
    Ok(())
}

/// The job that prints init's word accepting a swap of netd, and that word.
const CARRIER_FORGER: &str = "test_rs_log_carrier_forger";
const CARRIER_FORGED: &str =
    "init: swap netd: accepted: /tmp/swap/forged/netd replaces /system/bin/netd (pid 1)";

/// **Only init's word to `logd` can turn the network's readers away.** A job
/// prints the very line init says accepting a swap of netd; a reader connecting
/// after it is admitted, and `/log` carries the line under the job's runner and
/// no word from `logd` that it turns readers away.
pub fn carrier_forgery(c_bins: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let staged = logstream::stage(VIRTIO.config, "log-carrier-forgery", c_bins, rust_bins)?;
    let port = qemu::free_host_port();
    let options = BootOptions {
        profile: VIRTIO.profile,
        boot_image: Some(qemu::Staged::Written(staged.image.clone())),
        log_port: Some(port),
        ..Default::default()
    };
    let config = compile::repo_root().join(VIRTIO.config);
    let mut guest = QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
    let mut console = guest.boot_log().to_string();
    qemu::await_marker(&mut guest, &mut console, logstream::SERVING, "logd to open its port")?;
    let ran = guest.run_test(CARRIER_FORGER, Duration::from_secs(60));
    if ran.exit_code != Some(0) {
        return Err(format!("{CARRIER_FORGER} exited {:?}\n{}", ran.exit_code, ran.stdout));
    }
    let reader = logstream::reader(port, "log-carrier-forgery.txt")
        .map_err(|e| format!("a reader asking after a program printed init's word was not admitted: {e}"))?;
    if !reader.wait_for(CARRIER_FORGED, Duration::from_secs(60)) {
        return Err(format!("the served log never carried {CARRIER_FORGED:?}"));
    }
    let file = logstream::shut_down(guest, &mut console, &staged)?;
    if !reader.wait_ended(Duration::from_secs(60)) {
        return Err("the reader's connection had not ended once the guest was down".to_string());
    }
    let log = file.concat();
    if !bootlog::lines_of(&log, RUNNER).lines().any(|l| l == CARRIER_FORGED) {
        return Err(format!("/log carries no {CARRIER_FORGED:?} under {RUNNER:?}: nothing was forged"));
    }
    if bootlog::lines_of(&log, "logd").contains(toyos_logstream::CARRIER_LEAVING) {
        return Err(format!(
            "a program's line moved logd to turn readers away: /log carries {:?}",
            toyos_logstream::CARRIER_LEAVING
        ));
    }
    logstream::is_prefix_of(&reader.lines(), &file)?;
    eprintln!(
        "  [origin] {RUNNER:?} printed init's word accepting a swap of netd; a reader after it was \
         admitted, and logd turned nobody away"
    );
    let _ = std::fs::remove_file(&staged.image);
    Ok(())
}

/// **netd answers for its name, to its link and to nobody off it.** The host
/// stands on the guest's segment (`segment`) as a neighbour, [`NEIGHBOUR`]. Once
/// the guest holds its lease, the neighbour makes itself known (an ARP request
/// for the guest's address, which the guest answers) and then puts three
/// legacy resolvers' queries (RFC 6762 §6.7) on the wire:
///
/// 1. for this machine's name, from `127.0.0.1` — a source RFC 1122
///    §3.2.1.3 says a host MUST NOT send and MUST silently discard;
/// 2. for another name, from the neighbour;
/// 3. for this machine's name, from the neighbour.
///
/// Only the third is answered: the lease's address, the asker's ID and
/// question, a TTL of ten seconds, addressed to the neighbour. Silence is not
/// waited for — netd answers one socket's queries in the order they arrived,
/// through one socket's queue sent in order, so an answer to either earlier
/// query would be on the wire before the third one's.
///
/// The frames, the query and the reading of the answer are spelled here, byte
/// by byte from RFC 826, 791, 768 and 1035 §4.1, and not by `toyos_mdns`,
/// which is what wrote the answer.
pub fn mdns(c_bins: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let tap = segment::Tap::in_lane();
    let options = BootOptions { profile: VIRTIO.profile, segment: Some(tap.clone()), ..Default::default() };
    let config = compile::repo_root().join(VIRTIO.config);
    let mut guest = QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
    let mut console = guest.boot_log().to_string();
    qemu::await_marker(&mut guest, &mut console, "netd: DHCP: lease ", "netd's lease")?;
    let mut wire = tap.open()?;
    let deadline = || Instant::now() + Duration::from_secs(10);

    wire.send(&segment::arp_request(NEIGHBOUR_MAC, NEIGHBOUR, GUEST))?;
    let until = deadline();
    let guest_mac = loop {
        let frame = wire.next(until).map_err(|e| format!("no ARP reply for {GUEST:?} in 10 s: {e}"))?;
        if let Some(mac) = segment::arp_reply_for(&frame, GUEST) {
            break mac;
        }
    };

    // Where slirp delivers anything the guest sends to an address on its
    // network that is none of slirp's own: host loopback, at this port. Held
    // here so that no other process on the host is handed it.
    let stray = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| format!("bind: {e}"))?;
    let port = stray.local_addr().map_err(|e| format!("{e}"))?.port();
    let host = toyos_build::lan::HOSTNAME;
    let ask = |from: [u8; 4], payload: &[u8]| {
        segment::Udp {
            dst_mac: guest_mac,
            src_mac: NEIGHBOUR_MAC,
            src: (from, port),
            dst: (GUEST, MDNS_PORT),
            payload,
        }
        .frame()
    };
    const LOOPBACK_ID: u16 = 0x7f01;
    const OTHER_ID: u16 = 0x0bad;
    const OWN_ID: u16 = 0x5eed;
    let asked = Instant::now();
    wire.send(&ask([127, 0, 0, 1], &query(LOOPBACK_ID, host)))?;
    wire.send(&ask(NEIGHBOUR, &query(OTHER_ID, "some-other-host")))?;
    wire.send(&ask(NEIGHBOUR, &query(OWN_ID, host)))?;

    let until = deadline();
    let (answer, to) = loop {
        let frame = wire.next(until).map_err(|e| format!("no answer for {host}.local in 10 s: {e}"))?;
        let Some(udp) = segment::udp_in(&frame) else { continue };
        if udp.src != (GUEST, MDNS_PORT) || udp.payload.len() < 2 {
            continue;
        }
        match u16::from_be_bytes([udp.payload[0], udp.payload[1]]) {
            LOOPBACK_ID => {
                return Err(format!(
                    "a query from 127.0.0.1 was answered, to {:?}: {:02x?}",
                    udp.dst, udp.payload
                ));
            }
            OTHER_ID => return Err(format!("a query for another name was answered: {:02x?}", udp.payload)),
            OWN_ID => break (udp.payload.to_vec(), (udp.dst_mac, udp.dst)),
            _ => {}
        }
    };
    let took = asked.elapsed();
    let mut want = query(OWN_ID, host);
    // QR and AA, one question, one answer.
    want[2..8].copy_from_slice(&[0x84, 0x00, 0, 1, 0, 1]);
    want.extend_from_slice(&name(host));
    want.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 10, 0, 4]);
    want.extend_from_slice(&GUEST);
    if answer != want {
        return Err(format!("{host}.local was answered {answer:02x?}, and {want:02x?} is owed"));
    }
    if to != (NEIGHBOUR_MAC, (NEIGHBOUR, port)) {
        return Err(format!("{host}.local was answered to {to:02x?}, not to the neighbour that asked"));
    }
    drop(guest);
    serial::Serial::named("the boot", console.as_str()).must_be_clean()?;
    eprintln!(
        "  [mdns] {host}.local answered {GUEST:?} to an on-link neighbour in {} ms; 127.0.0.1 and \
         another name, nothing",
        took.as_millis()
    );
    Ok(())
}

/// The address slirp's DHCP gives the first guest on its network, and a
/// neighbour on the same /24 that is none of slirp's own addresses.
const GUEST: [u8; 4] = [10, 0, 2, 15];
const NEIGHBOUR: [u8; 4] = [10, 0, 2, 7];
/// A locally administered unicast address (IEEE 802 bit 1 of the first octet).
const NEIGHBOUR_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x0a, 0x00, 0x07];
/// RFC 6762 §3: the port every multicast DNS responder listens on.
const MDNS_PORT: u16 = 5353;

/// `<host>.local` as labels.
fn name(host: &str) -> Vec<u8> {
    let mut out = vec![host.len() as u8];
    out.extend_from_slice(host.as_bytes());
    out.extend_from_slice(b"\x05local\x00");
    out
}

/// One question, type A, class IN, from a port that is not 5353.
fn query(id: u16, host: &str) -> Vec<u8> {
    let mut out = vec![(id >> 8) as u8, id as u8, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    out.extend_from_slice(&name(host));
    out.extend_from_slice(&[0, 1, 0, 1]);
    out
}
