//! `/system/bin/inspect` against the four owners it reads, on the one boot that
//! runs them all (`tests/inspectcase`).
//!
//! **Every selector is judged by the exact set of paths it printed**, spelled
//! out here and not recomputed with the reader's own matcher: a `*` that
//! matches too much is a path in the answer this file did not name, and one
//! that matches too little is a named path missing from it. Values are judged
//! where the machine fixes them — QEMU's user network leases `10.0.2.15/24`,
//! and nothing on this boot plays audio — and read only for shape elsewhere.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use super::qemu::{self, await_marker, BootOptions, QemuInstance, TestResult};

/// A liveness guard on one job, never a verdict.
const CEILING: Duration = Duration::from_secs(60);

pub const CONFIG: &str = "tests/inspectcase";

/// The guest binary that holds the negative control.
pub const DENIED: &str = "inspect_denied";

/// Every path the reader answers for netd on a virtio NIC with a lease.
const NET: &[&str] = &[
    "net.driver",
    "net.lease.address",
    "net.lease.dns",
    "net.lease.held",
    "net.lease.router",
    "net.lease.server",
    "net.link.state",
    "net.mac",
    "net.piped.live",
    "net.piped.max",
    "net.sockets.listeners",
    "net.sockets.tcp",
    "net.sockets.udp",
];

pub fn boot(rust_bins: &[(String, Vec<u8>)]) -> Result<QemuInstance, String> {
    let bins: Vec<(String, Vec<u8>)> =
        rust_bins.iter().filter(|(name, _)| name == DENIED).cloned().collect();
    if bins.is_empty() {
        return Err(format!("{DENIED} was not built"));
    }
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG);
    let options = BootOptions { profile: qemu::Profile::Gop, ..Default::default() };
    let argv = qemu::profile_argv(&options);
    if !argv.iter().any(|a| a.contains("virtio-net")) || !argv.iter().any(|a| a.contains("virtio-sound")) {
        return Err("this test needs a virtio NIC and a virtio sound card".to_string());
    }
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);
    let mut console = qemu.boot_log().to_string();
    // netd says it is ready once the lease question is settled, which is when
    // `net.lease.*` has an answer to give.
    await_marker(&mut qemu, &mut console, "netd: ready, at most ", "netd to come up")?;
    await_marker(&mut qemu, &mut console, "compositor: ready", "the compositor to come up")?;
    Ok(qemu)
}

/// The `path = value` lines of a job's output, and nothing else the console
/// carried while it ran.
fn answer(result: &TestResult) -> BTreeMap<String, String> {
    result
        .stdout
        .lines()
        .filter_map(|line| line.split_once(" = "))
        .filter(|(path, _)| {
            path.contains('.')
                && path.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-' | ':' | '.'))
        })
        .map(|(path, value)| (path.to_string(), value.to_string()))
        .collect()
}

/// Run one job and require it exited `code`.
fn job(qemu: &mut QemuInstance, line: &str, code: i32) -> Result<TestResult, String> {
    let result = qemu.run_test(line, CEILING);
    if let Some(err) = &result.error {
        return Err(format!("`{line}`: {err}\n{}", result.stdout));
    }
    if result.exit_code != Some(code) {
        return Err(format!("`{line}` exited {:?}, not {code}:\n{}", result.exit_code, result.stdout));
    }
    eprintln!("  [inspectcase] $ {line}");
    for (path, value) in answer(&result) {
        eprintln!("  [inspectcase] {path} = {value}");
    }
    Ok(result)
}

/// The paths a job printed, against the exact set it must print.
fn exactly(line: &str, got: &BTreeMap<String, String>, want: &[&str]) -> Result<(), String> {
    let got_paths: Vec<&str> = got.keys().map(String::as_str).collect();
    let mut want: Vec<&str> = want.to_vec();
    want.sort();
    if got_paths != want {
        return Err(format!("`{line}` printed {got_paths:?}, and the selector names {want:?}"));
    }
    Ok(())
}

fn value<'a>(line: &str, got: &'a BTreeMap<String, String>, path: &str) -> Result<&'a str, String> {
    got.get(path).map(String::as_str).ok_or_else(|| format!("`{line}` printed no {path}"))
}

fn expect(line: &str, got: &BTreeMap<String, String>, path: &str, want: &str) -> Result<(), String> {
    match value(line, got, path)? {
        v if v == want => Ok(()),
        v => Err(format!("`{line}`: {path} = {v}, not {want}")),
    }
}

fn number(line: &str, got: &BTreeMap<String, String>, path: &str) -> Result<u64, String> {
    let v = value(line, got, path)?;
    v.parse().map_err(|_| format!("`{line}`: {path} = {v} is not a count"))
}

pub fn reads_its_owners(qemu: &mut QemuInstance) -> Result<(), String> {
    let line = "inspect net.*";
    let got = answer(&job(qemu, line, 0)?);
    exactly(line, &got, NET)?;
    expect(line, &got, "net.driver", "virtio-net")?;
    expect(line, &got, "net.link.state", "unreported")?;
    expect(line, &got, "net.lease.held", "true")?;
    expect(line, &got, "net.lease.address", "10.0.2.15/24")?;
    if !value(line, &got, "net.mac")?.starts_with("52:54:00:") {
        return Err(format!("`{line}`: net.mac is not QEMU's: {:?}", got["net.mac"]));
    }
    if number(line, &got, "net.piped.max")? == 0 {
        return Err(format!("`{line}`: netd holds no piped connection at all"));
    }

    // The pipe the owner asked for: `inspect` into `grep`, through the shell.
    let line = "shell -c inspect sound.* | grep periods";
    let got = answer(&job(qemu, line, 0)?);
    exactly(line, &got, &["sound.periods.drains", "sound.periods.submitted", "sound.periods.underruns"])?;
    // Nothing on this boot has played a period.
    expect(line, &got, "sound.periods.submitted", "0")?;

    let line = "inspect sound.*";
    let got = answer(&job(qemu, line, 0)?);
    exactly(
        line,
        &got,
        &[
            "sound.buffers",
            "sound.channels",
            "sound.device",
            "sound.period_frames",
            "sound.periods.drains",
            "sound.periods.submitted",
            "sound.periods.underruns",
            "sound.rate_hz",
            "sound.stream.clients",
            "sound.stream.state",
            "sound.wakes.late",
        ],
    )?;
    expect(line, &got, "sound.device", "virtio-sound")?;
    expect(line, &got, "sound.stream.state", "suspended")?;
    expect(line, &got, "sound.stream.clients", "0")?;

    let line = "inspect log.*";
    let got = answer(&job(qemu, line, 0)?);
    exactly(
        line,
        &got,
        &["log.records.lost", "log.stream", "log.volume.bytes", "log.volume.part", "log.volume.path", "log.volume.state"],
    )?;
    expect(line, &got, "log.volume.state", "writing")?;
    expect(line, &got, "log.stream", "off")?;
    if !value(line, &got, "log.volume.path")?.starts_with("/log/") {
        return Err(format!("`{line}`: log.volume.path is not on /log: {:?}", got["log.volume.path"]));
    }
    if number(line, &got, "log.volume.bytes")? == 0 {
        return Err(format!("`{line}`: logd has written nothing this boot"));
    }

    let line = "inspect display.*";
    let got = answer(&job(qemu, line, 0)?);
    exactly(
        line,
        &got,
        &[
            "display.cursor",
            "display.frames.composite_us",
            "display.frames.composited",
            "display.frames.damage_px",
            "display.frames.rects",
            "display.height",
            "display.width",
            "display.windows.max",
            "display.windows.open",
        ],
    )?;
    expect(line, &got, "display.windows.open", "0")?;
    if number(line, &got, "display.frames.composited")? == 0 {
        return Err(format!("`{line}`: the compositor has composited no frame"));
    }

    // A `*` in first place reaches every owner, and one in last place stops at
    // a whole segment: exactly one `state` from each owner that has one.
    // A `*` in first place reaches every owner and the kernel, and one in last
    // place stops at a whole segment: one `state` from each owner that has
    // one, and each partition's.
    let line = "inspect *.state";
    let got = answer(&job(qemu, line, 0)?);
    let (dev, owners): (BTreeMap<String, String>, BTreeMap<String, String>) =
        got.into_iter().partition(|(path, _)| path.starts_with("dev."));
    exactly(line, &owners, &["log.volume.state", "net.link.state", "sound.stream.state"])?;
    if dev.is_empty() {
        return Err(format!("`{line}` printed no partition's state"));
    }
    if let Some(path) =
        dev.keys().find(|p| !(p.starts_with("dev.part.") && p.ends_with(".state")))
    {
        return Err(format!("`{line}` printed {path}, which is no partition's state"));
    }

    inventory(qemu)?;

    // Exact, with no `*`, is one path and never a prefix.
    let line = "inspect net.link";
    let got = answer(&job(qemu, line, 1)?);
    exactly(line, &got, &[])?;

    // A malformed selector is refused by name and asks nobody.
    let line = "inspect net*";
    let result = job(qemu, line, 2)?;
    if !result.stdout.contains("a `*` is a whole segment") {
        return Err(format!("`{line}` was not refused by name:\n{}", result.stdout));
    }

    let result = job(qemu, &format!("test_rs_{DENIED}"), 0)?;
    for verdict in [
        "inspect denied: granted read netd, denied was refused by name",
        "inventory denied: granted read dev.*, denied was refused by the kernel",
    ] {
        if !result.stdout.contains(verdict) {
            return Err(format!("{DENIED} did not say {verdict:?}:\n{}", result.stdout));
        }
    }
    Ok(())
}

/// `inspect dev.*`: the kernel's inventory, judged where QEMU fixes it. The
/// virtio NIC is `1af4:1041` and netd holds it; the virtio sound card and the
/// framebuffer are classes soundd and the compositor hold; the Gop profile's
/// USB keyboard is on the xHCI; and the boot stick carries partitions this
/// kernel mounted.
fn inventory(qemu: &mut QemuInstance) -> Result<(), String> {
    let line = "inspect dev.*";
    let got = answer(&job(qemu, line, 0)?);
    if number(line, &got, "dev.cpus")? == 0 {
        return Err(format!("`{line}`: the machine has no CPU"));
    }
    if number(line, &got, "dev.memory.total_bytes")? == 0 {
        return Err(format!("`{line}`: the machine has no memory"));
    }
    let nic: Vec<&str> = got
        .iter()
        .filter(|(path, value)| path.starts_with("dev.pci.") && path.ends_with(".device") && *value == "1041")
        .map(|(path, _)| path.trim_end_matches(".device"))
        .collect();
    let [nic] = nic.as_slice() else {
        return Err(format!("`{line}`: {} functions are a virtio NIC, not one", nic.len()));
    };
    expect(line, &got, &format!("{nic}.vendor"), "1af4")?;
    expect(line, &got, &format!("{nic}.driver"), "netd")?;
    expect(line, &got, "dev.class.virtio-sound.holder", "soundd")?;
    expect(line, &got, "dev.class.framebuffer.holder", "compositor")?;
    if !got.iter().any(|(p, v)| p.starts_with("dev.pci.") && p.ends_with(".driver") && v == "kernel") {
        return Err(format!("`{line}`: no PCI function is driven by the kernel"));
    }
    if !got.iter().any(|(p, v)| p.starts_with("dev.usb.") && p.ends_with(".function") && v == "keyboard") {
        return Err(format!("`{line}`: no USB keyboard"));
    }
    if !got.keys().any(|p| p.starts_with("dev.block.")) {
        return Err(format!("`{line}`: no block device"));
    }
    if !got.iter().any(|(p, v)| p.starts_with("dev.part.") && p.ends_with(".state") && v == "mounted") {
        return Err(format!("`{line}`: no partition is mounted"));
    }
    Ok(())
}
