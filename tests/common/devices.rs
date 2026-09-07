//! The device boot: what `tests/metaldevicecase` measures, and how the same
//! boot is judged under QEMU and on the T14.
//!
//! **The two arms answer different questions and say so.** Under QEMU the
//! devices are emulated and every span is a fact about TCG, so what is judged
//! there is the plumbing: each job ran, each returned a measurement rather than
//! one of `metaldevices::Refused`'s codes, and the shutdown took the devices
//! down in order. On the T14 the same log is judged against
//! `tests/metal-profile.toml`, where the spans have ceilings.

use std::path::Path;

use toyos_build::metaldevices;
use toyos_build::metalprofile::Profile;

use super::metal;
use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

/// Every job the config's runner list names that measures something, in that
/// order. `reboot` is the list's last job and measures nothing.
///
/// Held to the committed config by [`the_config_runs_exactly_these_jobs`].
pub const JOBS: &[&str] = &["usbwrite", "usbread", "fbcheck", "fbfill", "fbread"];

/// The boot config, and the name every profile row for this boot is under.
pub const CONFIG: &str = "tests/metaldevicecase";
pub const BOOT: &str = "metaldevicecase";

const WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The T14's judge: every record the inventory owes, every span against its
/// ceiling, and the shutdown's own account of what it handed back.
pub fn on_metal(back: &metal::Readback) -> Result<(), String> {
    let root = super::compile::repo_root();
    let profile = Profile::load(&root).map_err(|why| why.to_string())?;
    let mut bad = metaldevices::unmet(back.loader().text(), back.kernel().text());

    for job in JOBS {
        let code = match back.exit_code(job) {
            Ok(code) => code,
            Err(why) => {
                bad.push(why);
                continue;
            }
        };
        match measurement(job, code) {
            Err(why) => bad.push(why),
            Ok(span) => {
                let name = format!("{job}.{}.span_us", back.label);
                if let Err(why) = profile.judge(&name, span) {
                    bad.push(why.to_string());
                }
            }
        }
    }

    // Printed whatever the verdict: these are the lines whoever tightens a
    // ceiling or writes an inventory row next has to read, and they exist only
    // in a log that came off the machine.
    eprintln!("  [devices] what {} answered:", back.label);
    for line in metaldevices::inventory(back.kernel().text()) {
        eprintln!("    {line}");
    }

    if bad.is_empty() {
        return Ok(());
    }
    Err(format!("{} finding(s):\n  {}", bad.len(), bad.join("\n  ")))
}

/// One job's exit code as the number it is, or why it is not one.
fn measurement(job: &str, code: i32) -> Result<u64, String> {
    if let Some(refused) = metaldevices::Refused::of(i64::from(code)) {
        return Err(format!("{job}: the job refused — {refused}"));
    }
    u64::try_from(code)
        .map_err(|_| format!("{job}: exited {code}, which is neither a span nor a named refusal"))
}

/// The QEMU arm: the plumbing, on a machine whose spans mean nothing.
///
/// Every device here is emulated and every one of them is faster or slower than
/// the laptop by an amount nobody has measured, so **no span is judged**. What
/// is judged is that the boot ran its whole job list, that each job answered
/// with a measurement rather than a refusal, that the NVMe census counted no
/// write, and that the shutdown emptied a cache and halted a controller before
/// it let the reset go — which is a control for the sequence and not for its
/// timing.
pub fn metal_device_probe(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let case = super::compile::repo_root().join(CONFIG);
    let mut qemu = QemuInstance::boot_with_options(
        &case,
        &[],
        &[],
        BootOptions { profile: qemu::Profile::Metal, qmp: true, ..Default::default() },
    );
    serial::Serial::boot(&qemu).must_be_clean()?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(WAIT));
    let _ = stop.reason();
    let tail = qemu.drain_serial(WAIT);
    let text = format!("{}{tail}", qemu.boot_log());
    let log = serial::Serial::named("the device boot", text.as_str());

    for job in JOBS {
        let head = format!("exit: {job} pid=");
        let line = text
            .lines()
            .filter(|l| l.contains(&head))
            .next_back()
            .ok_or_else(|| format!("no `{head}` record: {job} never ran, or never ended"))?;
        let code: i32 = line
            .split_once(" code=")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("unreadable exit record: {line:?}"))?;
        let span = measurement(job, code)?;
        eprintln!("  [devices] {job}: {span} us");
    }

    // **The positive control for the safety instrument**, and this machine is
    // the only one that can give it. On the T14 the internal disk reads
    // `Foreign` (`kernel/src/bcachefs_adapter.rs`: "Never written to, under any
    // circumstances") and the metal judge asserts `write=0`; a counter that
    // was stuck at zero would pass that and say nothing. Here the guest is
    // given an NVMe carrying a ToyOS volume of its own, mounts it, and writes
    // it — so the census has to *move*, and the mount record is what accounts
    // for the writes by name.
    // Either arm of `Storage`'s two writable ones, because which the guest
    // finds depends on whether an earlier boot in this lane already formatted
    // the disk — and both of them write it.
    const OWNED: &[&str] = &[
        "storage: mounted the ToyOS volume at block 0",
        "storage: block 0 designates this device for ToyOS",
    ];
    if !OWNED.iter().any(|said| text.contains(said)) {
        return Err(format!(
            "this guest's NVMe read as neither of {OWNED:?}, so it owns no volume there and the              control below would be asserting the wrong thing"
        ));
    }
    let census = text
        .lines()
        .find(|l| l.contains(metaldevices::NVME_CENSUS))
        .ok_or_else(|| format!("no {:?} record", metaldevices::NVME_CENSUS))?;
    for field in ["read=", "write="] {
        let counted: u64 = census
            .split(field)
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("the census names no {field}: {census:?}"))?;
        if counted == 0 {
            return Err(format!(
                "this guest owns the NVMe volume and mounted it, so the census owes a non-zero                  {field} — a counter that cannot move would pass the T14's `write=0` and mean                  nothing: {census:?}"
            ));
        }
    }
    // And the shutdown, in the order a device needs it: the cache is emptied
    // while the volume is still there, and the boot's own last word is still
    // the last thing in the log.
    log.must_say("usb-quiesce: disk 0 SYNCHRONIZE CACHE ok")?;
    log.must_say(toyos_build::bootlog::REBOOTING)?;
    let flushed = text
        .find("SYNCHRONIZE CACHE")
        .ok_or_else(|| "no cache flush in the shutdown".to_string())?;
    let last_word = text
        .rfind(toyos_build::bootlog::REBOOTING)
        .ok_or_else(|| "no reset word in the log".to_string())?;
    if flushed > last_word {
        return Err(format!(
            "the cache flush is recorded after {:?}, so on a machine whose log is a file it \
             would land after the boot's own last line and no metal verdict could read it",
            toyos_build::bootlog::REBOOTING
        ));
    }
    Ok(())
}

/// The runner's job list is the one this file names, under the symlinks that
/// give each job the name the kernel's `exit:` record carries.
///
/// **A plain function, called from the harness's registration checks**: this
/// test binary is `harness = false`, so a `#[test]` here would be compiled and
/// never run — which is a gate that cannot fail.
pub fn the_config_runs_exactly_these_jobs() {
    let at = super::compile::repo_root().join(CONFIG).join("system.toml");
    let config = std::fs::read_to_string(&at).expect("the device case's config");
    let args = config
        .lines()
        .find_map(|line| line.trim().strip_prefix("args = ["))
        .expect("the runner's job list");
    let named: Vec<&str> = args
        .trim_end_matches(']')
        .split(',')
        .map(|word| word.trim().trim_matches('"'))
        .filter(|word| !word.is_empty())
        .collect();
    assert_eq!(named.last(), Some(&"reboot"), "{named:?}");
    assert_eq!(&named[..named.len() - 1], JOBS);
    for job in JOBS {
        assert!(
            config.contains(&format!("\"bin/{job}\" = \"/system/bin/metalprobe\"")),
            "{job} has no symlink, so the runner would spawn nothing"
        );
    }
}
