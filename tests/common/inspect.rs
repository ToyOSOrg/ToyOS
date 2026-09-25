//! `/system/bin/inspect` against the four owners it reads, on the one boot that
//! runs them all (`tests/inspectcase`).
//!
//! **Every selector is judged by the exact set of paths it printed**, spelled
//! out here and not recomputed with the reader's own matcher: a `*` that
//! matches too much is a path in the answer this file did not name, and one
//! that matches too little is a named path missing from it. Values are judged
//! where the machine fixes them — QEMU's user network leases `10.0.2.15/24`,
//! nothing plays audio until `inspect_plays` does, and the NVMe disk this file
//! crafts has one partition free and one init grants — and read only for shape
//! elsewhere.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use super::qemu::{self, await_marker, BootOptions, QemuInstance, TestResult};

/// A liveness guard on one job, never a verdict.
const CEILING: Duration = Duration::from_secs(60);

pub const CONFIG: &str = "tests/inspectcase";

/// The guest binary that holds the negative control.
pub const DENIED: &str = "inspect_denied";
/// The guest binary that plays periods through soundd.
pub const PLAYS: &str = "inspect_plays";
/// The guest binary that sends `SYS_DEVICE_INVENTORY` its edges.
pub const BOUNDS: &str = "inventory_bounds";

/// The crafted disk's partition nobody holds.
const FREE: &str = "9D1E2F30-4A5B-4C6D-8E7F-0A1B2C3D4E5F";
/// The one init grants test-runner; mirrored in the config.
const GRANTED: &str = "B4C5D6E7-F809-4A1B-8C2D-3E4F5A6B7C8D";

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
        rust_bins.iter().filter(|(name, _)| [DENIED, PLAYS, BOUNDS].contains(&name.as_str())).cloned().collect();
    if bins.len() != 3 {
        return Err(format!("{DENIED}, {PLAYS} and {BOUNDS} were not all built"));
    }
    let nvme = super::lane::dir().join("inspect-disk.img");
    let mib = 1024 * 1024;
    super::partclaim::craft_plain_disk(
        &nvme,
        &[("free", mib, FREE), ("granted", mib, GRANTED)],
        96 * mib,
    )?;
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join(CONFIG);
    let options =
        BootOptions { profile: qemu::Profile::Gop, nvme_image: Some(nvme), ..Default::default() };
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

    // Periods through soundd, and the running sums grow by at least what it
    // took: each frame soundd took went out in a period it submitted.
    let result = job(qemu, &format!("test_rs_{PLAYS}"), 0)?;
    let said = "inspect plays: soundd took ";
    let taken: u64 = result
        .stdout
        .lines()
        .find_map(|l| l.split_once(said).map(|(_, rest)| rest.trim_end_matches(" frames")))
        .and_then(|n| n.trim().parse().ok())
        .ok_or_else(|| format!("{PLAYS} did not say how much soundd took:\n{}", result.stdout))?;
    if taken == 0 {
        return Err(format!("{PLAYS} proved soundd took nothing:\n{}", result.stdout));
    }
    let line = "inspect sound.*";
    let got = answer(&job(qemu, line, 0)?);
    let submitted = number(line, &got, "sound.periods.submitted")?;
    let period = number(line, &got, "sound.period_frames")?;
    if submitted.saturating_mul(period) < taken {
        return Err(format!(
            "`{line}`: {submitted} periods of {period} frames submitted, and soundd took {taken} \
             frames"
        ));
    }

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
    if let Some(path) = dev.keys().find(|p| !(p.starts_with("dev.disk.") && p.ends_with(".state"))) {
        return Err(format!("`{line}` printed {path}, which is no partition's state"));
    }

    inventory(qemu)?;

    let result = job(qemu, &format!("test_rs_{BOUNDS}"), 0)?;
    for verdict in [
        "inventory bounds: an empty buffer answers ",
        " records is refused whole",
        "inventory bounds: 1025 records is refused",
        "inventory bounds: a count whose length wraps is refused",
    ] {
        if !result.stdout.contains(verdict) {
            return Err(format!("{BOUNDS} did not say {verdict:?}:\n{}", result.stdout));
        }
    }

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

/// The value of every `holder.<pid>` under `at`.
fn holders<'a>(got: &'a BTreeMap<String, String>, at: &str) -> Vec<&'a str> {
    let under = format!("{at}.holder.");
    got.iter().filter(|(p, _)| p.starts_with(&under)).map(|(_, v)| v.as_str()).collect()
}

/// The `dev.disk.<id>.part<index>` whose unique GUID is `unique`.
fn partition<'a>(line: &str, got: &'a BTreeMap<String, String>, unique: &str) -> Result<&'a str, String> {
    let unique = unique.to_ascii_lowercase();
    let found: Vec<&str> = got
        .iter()
        .filter(|(p, v)| p.starts_with("dev.disk.") && p.ends_with(".unique") && **v == unique)
        .map(|(p, _)| p.trim_end_matches(".unique"))
        .collect();
    match found.as_slice() {
        [one] => Ok(one),
        _ => Err(format!("`{line}`: {} partitions are {unique}, not one", found.len())),
    }
}

/// `inspect dev.*`: the kernel's inventory, judged where QEMU fixes it. The
/// virtio NIC is `1af4:1041` and netd holds it; the virtio sound card and the
/// framebuffer are classes soundd and the compositor hold; the Gop profile's
/// USB keyboard is on the xHCI; the boot stick carries partitions this kernel
/// mounted; and of the crafted disk's two, one is free and test-runner holds
/// the other.
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
    expect(line, &got, &format!("{nic}.driver"), "claimed")?;
    for (at, want) in [
        (nic.to_string(), "netd"),
        ("dev.class.virtio-sound".to_string(), "soundd"),
        ("dev.class.framebuffer".to_string(), "compositor"),
    ] {
        if holders(&got, &at) != [want] {
            return Err(format!("`{line}`: {at} is held by {:?}, not {want}", holders(&got, &at)));
        }
    }
    if !got.iter().any(|(p, v)| p.starts_with("dev.pci.") && p.ends_with(".driver") && v == "kernel") {
        return Err(format!("`{line}`: no PCI function is driven by the kernel"));
    }
    if !got.iter().any(|(p, v)| p.starts_with("dev.usb.") && p.ends_with(".function") && v == "keyboard") {
        return Err(format!("`{line}`: no USB keyboard"));
    }
    if !got.keys().any(|p| p.starts_with("dev.disk.") && p.ends_with(".blocks")) {
        return Err(format!("`{line}`: no block device"));
    }
    if !got.iter().any(|(p, v)| p.starts_with("dev.disk.") && p.ends_with(".state") && v == "kernel") {
        return Err(format!("`{line}`: no partition is held by the kernel"));
    }
    let free = partition(line, &got, FREE)?;
    expect(line, &got, &format!("{free}.state"), "free")?;
    if !holders(&got, free).is_empty() {
        return Err(format!("`{line}`: {free} is free and held by {:?}", holders(&got, free)));
    }
    let granted = partition(line, &got, GRANTED)?;
    expect(line, &got, &format!("{granted}.state"), "claimed")?;
    if holders(&got, granted) != ["test-runner"] {
        return Err(format!("`{line}`: {granted} is held by {:?}, not test-runner", holders(&got, granted)));
    }
    Ok(())
}
