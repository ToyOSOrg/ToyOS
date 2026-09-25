//! A program's output in the log, under the name of the pipe it came out of:
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
/// it runs as `test-runner`'s child, on `test-runner`'s pipe.
pub const NONCE: &str = "log origin nonce 7d1f3a";
const ORIGIN_JOB: &str = "test_rs_log_origin";
const RUNNER: &str = "test-runner";

/// The flooding program, its line count, and its last line's head.
pub const FLOODER: &str = "test_rs_log_flood";
const FLOOD_LINES: usize = 81_920;
pub const FLOOD_DONE: &str = "flood done lines=";

/// The forger, and the exit it really has.
const FORGER: &str = "test_rs_log_forger";
const FORGER_CODE: i64 = 7;

/// **A program's line reaches `/log`, the served log and the console, and each
/// says whose it is.** On the console it is the bytes the program wrote; in the
/// file and on the stream it is the same line under `test-runner`'s name,
/// because that is the pipe it came out of. init's and `logd`'s own lines are
/// in the file under theirs.
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
    // The console: the bytes as written, and no head on them.
    if !ran.stdout.lines().any(|l| l.trim_end() == NONCE) {
        return Err(format!("the console never carried {NONCE:?} as written\n{}", ran.stdout));
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
        "  [origin] {NONCE:?} on the console as written, and under {RUNNER:?} in /log and on \
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
/// is in `/log` — as `test-runner`'s — and every judge reads the truth: the
/// kernel's exit record says 7, no kernel record carries the forged words, and
/// netd said nothing (this boot runs no netd).
pub fn forgery(c_bins: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let (ran, log) = one_job("tests/testcases", "log-program-forgery", FORGER, Duration::from_secs(60), c_bins, rust_bins)?;
    if ran.exit_code != Some(FORGER_CODE as i32) {
        return Err(format!("{FORGER} exited {:?}\n{}", ran.exit_code, ran.stdout));
    }
    let forged = bootlog::lines_of(&log, RUNNER);
    // Non-vacuity: every forgery reached the file.
    for words in [
        "exit: test_rs_log_forger pid=1 code=0 cpu=0ms",
        "[2026-09-24 10:00:00 1.000 cpu0] exit: test_rs_log_forger",
        "netd: DHCP: lease 10.9.9.9/24 forged",
        "Rebooting.",
    ] {
        if !forged.contains(words) {
            return Err(format!("/log carries no {RUNNER} line with {words:?}: nothing was forged\n{log}"));
        }
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
        "  [origin] four forgeries in /log, each under {RUNNER:?}; the exit judge read \
         {FORGER_CODE} and no kernel record carries a forged word"
    );
    Ok(())
}

/// **A flood is slowed, never dropped.** `test_rs_log_flood` writes 5 MiB of
/// numbered lines, two and a half times what its pipe holds, as fast as the
/// pipe takes them; every one of them is in `/log`, once, in order.
pub fn flood(c_bins: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let (ran, log) = one_job("tests/testcases", "log-program-flood", FLOODER, Duration::from_secs(300), c_bins, rust_bins)?;
    if ran.exit_code != Some(0) {
        return Err(format!("{FLOODER} exited {:?}", ran.exit_code));
    }
    let said = bootlog::lines_of(&log, RUNNER);
    let mut next = 0usize;
    let mut done = None;
    for line in said.lines() {
        // First: the last line opens with `flood ` too.
        if line.starts_with(FLOOD_DONE) {
            done = Some(line.to_string());
        } else if let Some(rest) = line.strip_prefix("flood ") {
            let Some(n) = rest.split(' ').next().and_then(|n| n.parse::<usize>().ok()) else {
                return Err(format!("/log carries a flood line with no number: {line:?}"));
            };
            if n != next {
                return Err(format!(
                    "/log carries flood line {n} where line {next} is owed: a line was lost or \
                     reordered"
                ));
            }
            next += 1;
        }
    }
    if next != FLOOD_LINES {
        return Err(format!("/log carries {next} of the flood's {FLOOD_LINES} lines"));
    }
    let done = done.ok_or("/log carries the flood's every line and not its last")?;
    eprintln!("  [origin] all {FLOOD_LINES} flood lines in /log, in order, once; {done}");
    Ok(())
}

/// The job `tests/logholdcase` holds the ring for, the line it says after its
/// records — which is the line that releases the hold — and how many records
/// it has the kernel write first.
const HOLD_JOB: &str = "test_rs_log_hold";
const HOLD_LINE: &str = "log hold: said after 192 records";
const HOLD_RECORDS: usize = 192;
/// The kernel's record of each of those.
const RETIRED: &str = "syscall 26 is retired";

/// **A program's line lands after every record written before it was read.**
/// `tests/logholdcase`'s `logd` reads no record until `test_rs_log_hold` says
/// its line, which it says after having the kernel write three batches of
/// records: the line is read while all of them are unread, and `/log` must
/// carry every one of them before it.
pub fn after_records(c_bins: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let (ran, log) =
        one_job("tests/logholdcase", "log-hold", HOLD_JOB, Duration::from_secs(60), c_bins, rust_bins)?;
    if ran.exit_code != Some(0) {
        return Err(format!("{HOLD_JOB} exited {:?}\n{}", ran.exit_code, ran.stdout));
    }
    if !bootlog::lines_of(&log, "logd").contains("reading the kernel's records again") {
        return Err(format!("/log never says logd's hold on the ring ended\n{log}"));
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
            "{after} of the {HOLD_RECORDS} records written before {HOLD_LINE:?} was read are after \
             it in /log"
        ));
    }
    eprintln!(
        "  [origin] {HOLD_LINE:?}, read with all {HOLD_RECORDS} of its records unread, is after \
         every one of them in /log"
    );
    Ok(())
}

/// The job that prints init's word accepting a swap of netd, and that word.
const CARRIER_FORGER: &str = "test_rs_log_carrier_forger";
const CARRIER_FORGED: &str =
    "init: swap netd: accepted: /tmp/swap/forged/netd replaces /system/bin/netd (pid 1)";

/// **Only init's pipe can turn the network's readers away.** A job prints the
/// very line init says accepting a swap of netd; a reader connecting after it
/// is admitted, and `/log` carries the line under the job's runner and no word
/// from `logd` that it turns readers away.
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
