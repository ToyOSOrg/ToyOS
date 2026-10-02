//! The device boot: what `tests/metaldevicecase` measures.

/// Every job the config's runner list names that measures something, in that
/// order. `reboot` is the list's last job and measures nothing.
///
/// Held to the committed config by [`the_config_runs_exactly_these_jobs`].
pub const JOBS: &[&str] = &["usbwrite", "usbread", "fbcheck", "fbfill", "fbread"];

pub const CONFIG: &str = "tests/metaldevicecase";
pub const BOOT: &str = "metaldevicecase";

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
