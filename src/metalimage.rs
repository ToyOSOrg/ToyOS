//! The images the metal profile builds: one committed boot config, armed one
//! way, carrying a job list that ends the boot.
//!
//! **A T14 boot has to end itself.** Nothing is on the console — a userland
//! write reaches `Backend::None` on a machine with no serial port — so the
//! stdin path every `tests/*case` but `jobcase` leaves the runner on parks
//! forever, and the loop refuses after `metal::return_secs`. What ends a boot
//! is `[programs.test-runner] args`: one binary name per job, then `reboot`.
//!
//! So a metal image is a committed boot config with that one field derived onto
//! it. The derivation is here, pure, because what it changes about the config
//! is what the T14 sees and nothing else may drift from it: the two authorities
//! a `reboot` job needs (`dup` to hand the applet a duplicate, `power` for the
//! reset), the applet itself, and the name that reaches it.

#![forbid(unsafe_code)]

use toml::Value;

/// The runner's manifest key, and the job that hands the machine back.
const RUNNER: &str = "test-runner";
const REBOOT: &str = "reboot";
const TOYBOX: &str = "toybox";
const REBOOT_LINK: &str = "bin/reboot";
const TOYBOX_PATH: &str = "/system/bin/toybox";

/// The two rights the last job needs. `power` is the reset; `dup` is what lets
/// the runner hand a duplicate down to the applet, which is not a `[programs]`
/// key and so has no row of its own.
const RUNNER_SYSCAP: [&str; 2] = ["dup", "power"];

/// Why a committed boot config cannot become a metal image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Underived {
    Toml(String),
    /// A config whose boot list has no runner: nothing in it could run a job,
    /// so nothing in it could end a boot.
    NoRunner(String),
}

impl std::fmt::Display for Underived {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Toml(why) => write!(f, "the boot config does not read as one: {why}"),
            Self::NoRunner(what) => write!(
                f,
                "the boot config {what}, so no job list could end its boot — and a T14 boot \
                 that does not end itself parks on a console nothing is on"
            ),
        }
    }
}

/// The committed config with the batch's job list derived onto it.
///
/// `jobs` are the words the runner takes, in order; `reboot` is appended here
/// rather than by every caller, because a list that did not end in one is a
/// boot the loop waits 360 s for and then refuses.
pub fn derive(config: &str, jobs: &[&str]) -> Result<String, Underived> {
    let mut root: Value = toml::from_str(config).map_err(|e| Underived::Toml(e.to_string()))?;
    let table = root.as_table_mut().ok_or_else(|| Underived::Toml("not a table".to_string()))?;

    let starts = table
        .get("boot")
        .and_then(|b| b.get("start"))
        .and_then(Value::as_array)
        .map(|a| a.iter().any(|v| v.as_str() == Some(RUNNER)))
        .unwrap_or(false);
    if !starts {
        return Err(Underived::NoRunner(format!("does not start `{RUNNER}`")));
    }

    let programs = table
        .entry("programs")
        .or_insert_with(|| Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| Underived::Toml("`programs` is not a table".to_string()))?;
    if !programs.contains_key(RUNNER) {
        return Err(Underived::NoRunner(format!("declares no `[programs.{RUNNER}]` row")));
    }
    // The applet the last job runs. A config that already builds it keeps
    // whatever its row says; one that does not gets the bare row, which is what
    // `tests/jobcase` carries.
    programs.entry(TOYBOX).or_insert_with(|| Value::Table(toml::map::Map::new()));

    let runner = programs
        .get_mut(RUNNER)
        .and_then(Value::as_table_mut)
        .ok_or_else(|| Underived::Toml(format!("`programs.{RUNNER}` is not a table")))?;
    let mut args: Vec<Value> = jobs.iter().map(|j| Value::String((*j).to_string())).collect();
    args.push(Value::String(REBOOT.to_string()));
    runner.insert("args".to_string(), Value::Array(args));

    let syscap = runner
        .entry("syscap")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| Underived::Toml(format!("`programs.{RUNNER}.syscap` is not a list")))?;
    for right in RUNNER_SYSCAP {
        if !syscap.iter().any(|v| v.as_str() == Some(right)) {
            syscap.push(Value::String(right.to_string()));
        }
    }

    let symlinks = table
        .entry("symlinks")
        .or_insert_with(|| Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| Underived::Toml("`symlinks` is not a table".to_string()))?;
    symlinks.insert(REBOOT_LINK.to_string(), Value::String(TOYBOX_PATH.to_string()));

    let mut out = String::from(
        "# Derived by `toyos_build::metalimage::derive` — not committed, not edited.\n\
         # A metal boot ends itself: the job list below, then `reboot`.\n",
    );
    out.push_str(&toml::to_string(&root).map_err(|e| Underived::Toml(e.to_string()))?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every committed config the metal profile derives from, read from the
    /// tree so the transform is exercised on the real text rather than on a
    /// fixture that agrees with it today.
    const CONFIGS: &[&str] =
        &["tests/testcases", "tests/jobcase", "tests/metalcase", "tests/latencycase"];

    fn derived(dir: &str, jobs: &[&str]) -> String {
        let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir).join("system.toml");
        let text = std::fs::read_to_string(&at).expect("a committed boot config");
        derive(&text, jobs).unwrap_or_else(|why| panic!("{}: {why}", at.display()))
    }

    /// The whole point of the derivation, on every config it is asked of: the
    /// runner ends the boot, and it holds the two rights the last job needs.
    #[test]
    fn every_derived_config_ends_its_own_boot() {
        for dir in CONFIGS {
            let out = derived(dir, &["test_rs_mkdir_cap"]);
            let parsed: Value = toml::from_str(&out).expect("the derived config parses");
            let runner = &parsed["programs"][RUNNER];
            let args: Vec<&str> =
                runner["args"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
            assert_eq!(args, ["test_rs_mkdir_cap", REBOOT], "{dir}");
            let syscap: Vec<&str> = runner["syscap"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            for right in RUNNER_SYSCAP {
                assert!(syscap.contains(&right), "{dir}: {syscap:?} lacks {right}");
            }
            assert_eq!(parsed["symlinks"][REBOOT_LINK].as_str(), Some(TOYBOX_PATH), "{dir}");
            assert!(parsed["programs"].get(TOYBOX).is_some(), "{dir} builds no {TOYBOX}");
        }
    }

    /// What the config already declares survives: the derivation adds the job
    /// list, and a right or a symlink it did not put there is not its to drop.
    #[test]
    fn nothing_the_config_declared_is_dropped() {
        let out = derived("tests/testcases", &[]);
        let parsed: Value = toml::from_str(&out).unwrap();
        let syscap: Vec<&str> = parsed["programs"][RUNNER]["syscap"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        // `tests/testcases` declares all five; the derivation adds neither a
        // sixth spelling of the two it needs nor takes the other three away.
        for right in ["device", "dup", "logread", "power", "roster"] {
            assert_eq!(
                syscap.iter().filter(|r| **r == right).count(),
                1,
                "{right} in {syscap:?}"
            );
        }
        assert_eq!(parsed["symlinks"]["bin/echo"].as_str(), Some(TOYBOX_PATH));
        assert!(parsed["programs"].get("soundd").is_some());
    }

    /// A config with nothing to run a job is refused by name rather than
    /// producing an image that parks the machine for 360 s.
    #[test]
    fn a_config_with_no_runner_is_refused_by_name() {
        let refusal = derive("[boot]\nstart = [\"logd\"]\n", &[]).unwrap_err();
        assert_eq!(refusal, Underived::NoRunner("does not start `test-runner`".to_string()));
        let said = refusal.to_string();
        assert!(said.contains("parks on a console nothing is on"), "{said}");

        let refusal =
            derive("[boot]\nstart = [\"test-runner\"]\n[programs.logd]\n", &[]).unwrap_err();
        assert!(matches!(refusal, Underived::NoRunner(_)), "{refusal:?}");
    }
}
