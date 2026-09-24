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
use super::{compile, serial};

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
    reader.wait_ended(Duration::from_secs(60));

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

/// Boot `tests/testcases` on a staged image, run `job` with `timeout`, shut
/// down, and hand back what it said and the whole of its `/log`.
fn one_job(
    name: &str,
    job: &str,
    timeout: Duration,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(qemu::TestResult, String), String> {
    let staged = logstream::stage("tests/testcases", name, c_bins, rust_bins)?;
    let options = BootOptions {
        boot_image: Some(qemu::Staged::Written(staged.image.clone())),
        ..Default::default()
    };
    let config = compile::repo_root().join("tests/testcases");
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
    let (ran, log) = one_job("log-program-forgery", FORGER, Duration::from_secs(60), c_bins, rust_bins)?;
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
    let (ran, log) = one_job("log-program-flood", FLOODER, Duration::from_secs(300), c_bins, rust_bins)?;
    if ran.exit_code != Some(0) {
        return Err(format!("{FLOODER} exited {:?}", ran.exit_code));
    }
    let said = bootlog::lines_of(&log, RUNNER);
    let mut next = 0usize;
    let mut done = None;
    for line in said.lines() {
        if let Some(rest) = line.strip_prefix("flood ") {
            let Some(n) = rest.split(' ').next().and_then(|n| n.parse::<usize>().ok()) else {
                continue;
            };
            if n != next {
                return Err(format!(
                    "/log carries flood line {n} where line {next} is owed: a line was lost or \
                     reordered"
                ));
            }
            next += 1;
        } else if line.starts_with(FLOOD_DONE) {
            done = Some(line.to_string());
        }
    }
    if next != FLOOD_LINES {
        return Err(format!("/log carries {next} of the flood's {FLOOD_LINES} lines"));
    }
    let done = done.ok_or("/log carries the flood's every line and not its last")?;
    eprintln!("  [origin] all {FLOOD_LINES} flood lines in /log, in order, once; {done}");
    Ok(())
}

/// **netd answers for its name.** A legacy resolver's query (RFC 6762 §6.7) for
/// `toyos-t14.local`, sent from the host through a forward onto the guest's
/// multicast DNS port once the guest holds its lease, is answered with the
/// lease's address, the asker's ID and question, and a TTL of ten seconds; a
/// query for another name is not answered at all.
///
/// The query and the reading of the answer are spelled here, byte by byte from
/// RFC 1035 §4.1, and not by `toyos_mdns`, which is what wrote the answer.
pub fn mdns(c_bins: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let port = qemu::free_udp_host_port();
    let options = BootOptions { profile: VIRTIO.profile, mdns_port: Some(port), ..Default::default() };
    let config = compile::repo_root().join(VIRTIO.config);
    let mut guest = QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
    let mut console = guest.boot_log().to_string();
    qemu::await_marker(&mut guest, &mut console, "netd: DHCP: lease ", "netd's lease")?;

    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| format!("bind: {e}"))?;
    socket.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| format!("{e}"))?;
    let asked = Instant::now();
    socket
        .send_to(&query(0x5eed, toyos_build::lan::HOSTNAME), (Ipv4Addr::LOCALHOST, port))
        .map_err(|e| format!("send the query: {e}"))?;
    let mut answer = [0u8; 512];
    let n = socket
        .recv(&mut answer)
        .map_err(|e| format!("no answer for {}.local in 10 s: {e}", toyos_build::lan::HOSTNAME))?;
    let took = asked.elapsed();
    let got = &answer[..n];
    let mut want = query(0x5eed, toyos_build::lan::HOSTNAME);
    // QR and AA, one question, one answer.
    want[2..8].copy_from_slice(&[0x84, 0x00, 0, 1, 0, 1]);
    want.extend_from_slice(&name(toyos_build::lan::HOSTNAME));
    want.extend_from_slice(&[0, 1, 0, 1, 0, 0, 0, 10, 0, 4, 10, 0, 2, 15]);
    if got != want {
        return Err(format!("{}.local was answered {got:02x?}, and {want:02x?} is owed", toyos_build::lan::HOSTNAME));
    }

    // Silence is not waited for: the other name is asked, then this one again,
    // down one forward netd reads in order, so the next answer is the third
    // question's unless the other name was answered.
    socket
        .send_to(&query(0x0bad, "some-other-host"), (Ipv4Addr::LOCALHOST, port))
        .map_err(|e| format!("send the other name's query: {e}"))?;
    socket
        .send_to(&query(0x5eee, toyos_build::lan::HOSTNAME), (Ipv4Addr::LOCALHOST, port))
        .map_err(|e| format!("send the third query: {e}"))?;
    let n = socket
        .recv(&mut answer)
        .map_err(|e| format!("no answer for {}.local asked again, in 10 s: {e}", toyos_build::lan::HOSTNAME))?;
    if answer[..2] != [0x5e, 0xee] {
        return Err(format!("a query for another name was answered: {:02x?}", &answer[..n]));
    }
    drop(guest);
    serial::Serial::named("the boot", console.as_str()).must_be_clean()?;
    eprintln!(
        "  [mdns] {}.local answered 10.0.2.15 in {} ms; another name, nothing",
        toyos_build::lan::HOSTNAME,
        took.as_millis()
    );
    Ok(())
}

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
