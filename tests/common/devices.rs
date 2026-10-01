//! The device boot: what `tests/metaldevicecase` measures, and how the same
//! boot is judged under QEMU and on the T14.
//!
//! **The two arms answer different questions and say so.** Under QEMU the
//! devices are emulated and every span is a fact about TCG, so what is judged
//! there is the plumbing: each job ran, each returned a measurement rather than
//! one of `metaldevices::Refused`'s codes, and the shutdown took the devices
//! down in order.

use toyos_build::metaldevices;

use super::metal;

/// Every job the config's runner list names that measures something, in that
/// order. `reboot` is the list's last job and measures nothing.
///
/// Held to the committed config by [`the_config_runs_exactly_these_jobs`].
pub const JOBS: &[&str] = &["usbwrite", "usbread", "fbcheck", "fbfill", "fbread"];

pub const CONFIG: &str = "tests/metaldevicecase";
pub const BOOT: &str = "metaldevicecase";

pub fn on_metal(back: &metal::Readback) -> Result<(), String> {
    let mut bad = metaldevices::unmet(back.loader().text(), back.log().text());

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
                bad.extend(back.measured(&format!("{job}.{}.span_us", back.label), span).err());
            }
        }
    }

    eprintln!("  [devices] what {} answered:", back.label);
    for line in metaldevices::inventory(back.log().text()) {
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
