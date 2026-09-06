//! The instrument every guest in this project is measured with, declared once.
//!
//! The measurement this exists for: on one runner image, one commit and one
//! accelerator, `desktop_typing_damage` is red on
//! QEMU 8.2.2 and green on 11.0.3, and `usb_storage_shapes` with it. So the
//! QEMU version is not a detail of the environment — it decides verdicts, and
//! a job that does not say which one it ran produces a number nobody can
//! compare with another.
//!
//! `.github/qemu-version` is that declaration. CI reads it from
//! `.github/instrument.sh` and **reds** on a disagreement, because `debian:sid`
//! is a rolling release and the alternative is an instrument that moves out
//! from under every recorded measurement in silence. This host reads it and
//! **notes** a disagreement, because brew moves QEMU when it feels like it and
//! a build must not stop for that — but the dev host is where
//! `tests/audio-baseline.toml` was recorded, so it drifting is the same fact
//! about the same comparison and has to be visible.

use std::path::Path;
use std::process::Command;

/// The QEMU every guest in CI runs, and the one this project's recorded numbers
/// were taken on.
///
/// Comment lines and blanks are stripped, so the file can explain itself to the
/// next reader; `.github/instrument.sh` strips the same two things with `grep`
/// and `tr`.
pub fn declared_qemu_version(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(".github/qemu-version")).ok()?;
    let version: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("")
        .split_whitespace()
        .collect();
    (!version.is_empty()).then_some(version)
}

/// The dated Debian archive the hosted guests install from, so a rolling
/// release cannot move the instrument between two runs of the same tree.
pub fn declared_apt_snapshot(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(".github/apt-snapshot")).ok()?;
    let stamp: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("")
        .split_whitespace()
        .collect();
    (!stamp.is_empty()).then_some(stamp)
}

/// What `qemu-system-x86_64 --version` says, or `None` where it did not answer
/// in the shape this reads.
///
/// One `--version` run and not a package query: the binary on `PATH` is the one
/// a boot will use, and no packaging system on any of the three hosts this runs
/// on answers for that.
pub fn host_qemu_version() -> Option<String> {
    let out = Command::new("qemu-system-x86_64").arg("--version").output().ok()?;
    parse_qemu_version(&String::from_utf8_lossy(&out.stdout))
}

/// `QEMU emulator version 11.0.3 (Debian 1:11.0.3+ds-1)` → `11.0.3`.
fn parse_qemu_version(text: &str) -> Option<String> {
    let first = text.lines().next()?;
    let rest = first.strip_prefix("QEMU emulator version ")?;
    let version = rest.split_whitespace().next()?;
    (!version.is_empty()).then(|| version.to_string())
}

/// The line `cargo run` prints when this host is not the instrument the
/// project's numbers were taken on, and nothing at all when it is.
pub fn qemu_version_note(root: &Path) -> Option<String> {
    let want = declared_qemu_version(root)?;
    let have = host_qemu_version()?;
    (have != want).then(|| {
        format!(
            "Note: this host runs QEMU {have} and .github/qemu-version declares {want} — \
             CI's guests and tests/audio-baseline.toml are on {want}, and the QEMU version \
             has been measured to decide test outcomes (`desktop_typing_damage` and \
             `usb_storage_shapes` are red on 8.2.2 and green on 11.0.3, same image, same \
             commit, same accelerator). Nothing here is broken; a comparison across the two is."
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// The workflows whose verdict somebody acts on. `probe-*.yml` are
    /// throwaway measurement branches and are not
    /// on this list; `toolchain.yml` installs QEMU for `check_prerequisites`
    /// and boots nothing.
    const GATES: &[&str] = &["ci.yml", "gate-a.yml"];

    /// Every `<job>:` block of a workflow, crudely and on purpose.
    ///
    /// Deliberately not a YAML parser: the shape is fixed — two spaces, a
    /// name, a colon, end of line — and anything else is a file to name
    /// rather than a shape to accommodate.
    fn jobs(text: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let mut in_jobs = false;
        for line in text.lines() {
            if line == "jobs:" {
                in_jobs = true;
                continue;
            }
            if !in_jobs {
                continue;
            }
            let is_header = line.starts_with("  ")
                && !line.starts_with("   ")
                && line.ends_with(':')
                && line[2..line.len() - 1].chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            if is_header {
                out.push((line[2..line.len() - 1].to_string(), String::new()));
            } else if let Some(last) = out.last_mut() {
                last.1.push_str(line);
                last.1.push('\n');
            }
        }
        out
    }

    /// A job that installs QEMU is a job that boots a guest, and one that boots
    /// a guest without naming its instrument produces a verdict nobody can
    /// compare with another.
    ///
    /// The rule is here rather than in one workflow's review because the way
    /// this hides is that a workflow reads perfectly well and never says what
    /// it is comparing against — gate A ran QEMU 8.2.2 against every other
    /// guest in CI on 11.0.3 for as long as that file existed.
    fn nameless(text: &str) -> Vec<String> {
        jobs(text)
            .into_iter()
            .filter(|(_, body)| body.contains("qemu-system-x86"))
            .filter(|(_, body)| !body.contains("instrument.sh"))
            .map(|(name, _)| name)
            .collect()
    }

    #[test]
    fn every_gate_that_boots_a_guest_names_its_instrument() {
        let root = repo_root();
        let mut bad = Vec::new();
        for file in GATES {
            let path = root.join(".github/workflows").join(file);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{} is a gate and is not readable: {e}", path.display()));
            // A scan that found no job at all would report every gate clean,
            // which is the shape this rule exists to refuse.
            let booting = jobs(&text).into_iter().filter(|(_, b)| b.contains("qemu-system-x86"));
            assert!(
                booting.count() > 0,
                "{file} is on the list because it boots guests, and the job scan found none — \
                 the scan is wrong, or the file no longer belongs on it"
            );
            for job in nameless(&text) {
                bad.push(format!("{file}: `{job}` installs QEMU and never runs instrument.sh"));
            }
        }
        assert!(
            bad.is_empty(),
            "a job that boots a guest without declaring its QEMU is a third instrument, \
             and that is invisible in a diff:\n  {}",
            bad.join("\n  ")
        );
    }

    /// Every `.github` file that reaches the snapshot archive names the one
    /// date `.github/apt-snapshot` declares.
    ///
    /// The date cannot live in one place: `deps` runs before the checkout,
    /// because `actions/checkout` wants git and the image has none, so each
    /// step carries its own copy of the URL and there is nothing for them to
    /// read it from. Copies of a date drift silently — two shards measuring
    /// two instruments reads exactly like a flaky test — so the copies are
    /// held here instead.
    #[test]
    fn every_snapshot_url_names_the_declared_date() {
        let root = repo_root();
        let want = declared_apt_snapshot(&root).expect(".github/apt-snapshot declares no date");
        const MARK: &str = "snapshot.debian.org/archive/debian/";
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(root.join(".github/workflows"))
            .expect(".github/workflows is not readable")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        files.push(root.join(".github/ci-image/Dockerfile"));
        files.sort();

        let mut seen = 0usize;
        let mut bad = Vec::new();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for line in text.lines() {
                let Some(rest) = line.split(MARK).nth(1) else { continue };
                // The Dockerfile builds the URL from a shell variable it read
                // out of the declaration itself, so there is no literal date.
                if rest.starts_with('$') {
                    continue;
                }
                seen += 1;
                let stamp: String =
                    rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                if stamp != want {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    bad.push(format!("{name}: {stamp}"));
                }
            }
        }
        assert!(
            seen > 0,
            "no file reaches the snapshot archive, so either the pin is gone and the hosted \
             guests are back on sid-as-it-stands, or this scan no longer finds it"
        );
        assert!(
            bad.is_empty(),
            "these install from a date .github/apt-snapshot does not declare ({want}):\n  {}",
            bad.join("\n  ")
        );
    }

    /// A gate job that boots a guest installs QEMU from the pinned archive.
    ///
    /// Naming the instrument and then taking whatever the mirror shipped that
    /// afternoon is the failure this pairs with: `instrument.sh` would refuse
    /// the run, correctly, and the tree would be told nothing about why.
    #[test]
    fn every_gate_that_boots_a_guest_installs_from_the_snapshot() {
        let root = repo_root();
        let mut bad = Vec::new();
        for file in GATES {
            let path = root.join(".github/workflows").join(file);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{} is a gate and is not readable: {e}", path.display()));
            for (name, body) in jobs(&text) {
                let installs_qemu =
                    body.contains("apt-get install") && body.contains("qemu-system-x86");
                if installs_qemu && !body.contains("snapshot.debian.org") {
                    bad.push(format!("{file}: `{name}` installs QEMU from sid as it stands"));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "a rolling release decides what these measure with:\n  {}",
            bad.join("\n  ")
        );
    }

    /// Teeth, run rather than argued: the tree cannot contain the workflow this
    /// rule is written against, so the rule is shown to refuse one.
    #[test]
    fn the_job_scan_refuses_a_job_that_boots_without_saying_what_with() {
        let good = concat!(
            "jobs:\n",
            "  a:\n    steps:\n",
            "      - run: apt-get install qemu-system-x86\n",
            "      - run: .github/instrument.sh\n",
        );
        assert!(nameless(good).is_empty());

        let bad = concat!(
            "jobs:\n",
            "  a:\n    steps:\n      - run: .github/instrument.sh\n",
            "  b:\n    steps:\n      - run: apt-get install qemu-system-x86\n",
        );
        assert_eq!(nameless(bad), ["b"]);

        assert_eq!(jobs(bad).iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    /// Every `runs-on:` value a workflow declares, in source order.
    ///
    /// Not a YAML parser, for the reason [`jobs`] is not one: the shape is
    /// fixed — a `runs-on:` key and the rest of its line, a bare label or a
    /// flow sequence — and anything else is a file to name rather than a shape
    /// to accommodate. What this closes is that one spelling; a block sequence
    /// under `runs-on:` is the form it walks past, and
    /// [`the_runner_scan_reads_the_key_and_not_the_prose_around_it`] asserts
    /// that it does.
    fn runs_on(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(|l| l.trim_start().strip_prefix("runs-on:"))
            .map(|v| v.trim().to_string())
            .collect()
    }

    /// **No workflow names a self-hosted label.** The owner decommissioned the
    /// T14 as a runner, so every lane is GitHub-hosted and there is no machine
    /// behind `self-hosted` or `toyos` to answer one — a job that named either
    /// would queue until it timed out, silently, since a label nothing offers
    /// is not an error to Actions.
    #[test]
    fn no_workflow_asks_for_a_runner_this_project_does_not_have() {
        let dir = repo_root().join(".github/workflows");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect(".github/workflows is not readable")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "yml"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "the workflow scan found no workflow, so it is wrong");

        let mut bad = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("{} decides where CI runs: {e}", path.display()));
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            for label in self_hosted(&text) {
                bad.push(format!("{name}: runs-on: {label}"));
            }
        }
        assert!(
            bad.is_empty(),
            "every lane is GitHub-hosted and these name a runner nothing offers, so they \
             would queue until they timed out:\n  {}",
            bad.join("\n  ")
        );
    }

    /// The `runs-on:` values naming a runner this project does not have.
    fn self_hosted(text: &str) -> Vec<String> {
        runs_on(text)
            .into_iter()
            .filter(|v| v.contains("self-hosted") || v.contains("toyos"))
            .collect()
    }

    /// The scan, shown refusing the shape it is written against, and shown
    /// walking past the block sequence it does not read.
    #[test]
    fn the_runner_scan_reads_the_key_and_not_the_prose_around_it() {
        let good = concat!(
            "jobs:\n",
            "  # runs-on: [self-hosted, toyos] is a comment and not a key\n",
            "  a:\n    runs-on: ubuntu-24.04\n",
            "  b:\n    runs-on: macos-latest\n",
        );
        assert_eq!(runs_on(good), ["ubuntu-24.04", "macos-latest"]);
        assert!(self_hosted(good).is_empty());

        let bad = concat!("jobs:\n", "  a:\n    runs-on: [self-hosted, Linux, X64, toyos]\n");
        assert_eq!(self_hosted(bad), ["[self-hosted, Linux, X64, toyos]"]);

        // The form this spelling does not reach, asserted so that widening the
        // scan reds here instead of leaving this sentence unchecked.
        let walked = concat!("jobs:\n", "  a:\n    runs-on:\n      - self-hosted\n      - toyos\n");
        assert!(self_hosted(walked).is_empty());
    }

    /// The second axis of the same job, held together for the same reason.
    ///
    /// Which *names* a run renders the price verdict for is decided by
    /// [`crate::durations::TIER_BASE_FLAG`] and by the two event expressions
    /// that fill it. Both failure directions are silent in the file that
    /// carries them: drop the flag and every pull request and every merge-queue
    /// composition quietly becomes the nightly, reding on names nobody in them
    /// touched; drop one of the two event expressions and that event quietly
    /// stops narrowing at all, with the workflow still reading perfectly.
    /// `merge_group` in particular is the lane the verdict is *rendered* on, so
    /// losing its base is losing the whole change.
    #[test]
    fn the_names_a_landing_is_judged_on_come_from_the_event_that_produced_it() {
        let path = repo_root().join(".github/workflows/ci.yml");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is a gate and is not readable: {e}", path.display()));
        let (_, durations) = jobs(&text)
            .into_iter()
            .find(|(name, _)| name == "durations")
            .expect("ci.yml renders the duration verdict in a job called `durations`");
        assert!(
            durations.contains(crate::durations::TIER_BASE_FLAG),
            "the `durations` job no longer passes {:?}, so it renders the whole tier verdict on \
             every pull request and every merge-queue composition — the state that dequeued \
             composition 32550410305 on a name nothing in it had touched",
            crate::durations::TIER_BASE_FLAG
        );
        for base in ["github.event.merge_group.base_sha", "github.event.pull_request.base.sha"] {
            assert!(
                durations.contains(base),
                "the `durations` job stopped reading {base}, so that event names no base and \
                 silently falls back to the nightly's whole-tree verdict"
            );
        }
        assert!(
            durations.contains("fetch-depth: 0"),
            "reading the base's `tests/toyos.rs` needs the base commit in the clone, and \
             actions/checkout leaves a depth-1 one without it"
        );
    }

    /// A workflow's top-level `pull_request:` trigger and the `branches:`
    /// line immediately beneath it, if any — the same crude shape [`jobs`]
    /// and [`runs_on`] read rather than a YAML parser. `None` means the file
    /// has no `pull_request:` trigger at all.
    fn pull_request_branches(text: &str) -> Option<String> {
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            if line == "  pull_request:" {
                return Some(
                    lines
                        .next()
                        .and_then(|l| l.trim_start().strip_prefix("branches:"))
                        .map(|v| v.trim().to_string())
                        .unwrap_or_default(),
                );
            }
        }
        None
    }

    /// Integration branches (`metal` today) develop locally and fast; CI
    /// starts only once the work reaches the pull request to `main`. A
    /// `pull_request:` trigger with no `branches:` filter runs its whole job
    /// list on a pull request whatever its base — a draft PR into `metal` ran
    /// the full CI before this rule existed. The `seen` count is the teeth:
    /// a workflow that gains or drops the trigger without updating it is the
    /// silent drift this exists to catch.
    #[test]
    fn every_pull_request_trigger_runs_only_against_main() {
        let dir = repo_root().join(".github/workflows");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect(".github/workflows is not readable")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "yml"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "the workflow scan found no workflow, so it is wrong");

        let mut seen = 0usize;
        let mut bad = Vec::new();
        for path in &files {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("{} decides where CI runs: {e}", path.display()));
            let Some(branches) = pull_request_branches(&text) else { continue };
            seen += 1;
            if branches != "[main]" {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                bad.push(format!("{name}: branches: {branches:?}"));
            }
        }
        assert_eq!(
            seen, 4,
            "ci.yml, host-tests.yml, landing.yml and toolchain.yml are the four workflows this \
             rule was written for; the scan found a different count, so a workflow gained or \
             lost a `pull_request:` trigger and this count needs to move with it"
        );
        assert!(
            bad.is_empty(),
            "a draft PR against an integration branch runs the whole workflow before the work \
             is ready for main:\n  {}",
            bad.join("\n  ")
        );
    }

    /// The declaration is read by a shell and by this crate, so both have to
    /// agree that it holds one version and nothing else.
    #[test]
    fn the_declared_version_is_a_version() {
        let declared =
            declared_qemu_version(&repo_root()).expect(".github/qemu-version declares a version");
        assert!(
            declared.split('.').count() >= 2
                && declared.chars().all(|c| c.is_ascii_digit() || c == '.'),
            "{declared:?} is not a QEMU version"
        );
    }

    #[test]
    fn the_script_that_reads_it_is_runnable() {
        let path = repo_root().join(".github/instrument.sh");
        let meta = std::fs::metadata(&path).expect(".github/instrument.sh is there");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(
                meta.permissions().mode() & 0o111 != 0,
                "every guest job invokes it by path, so a lost exec bit reds all thirteen"
            );
        }
        let _ = meta;
    }

    /// Every file that computes the toolchain release tag. `toolchain.yml`
    /// mints it; the rest ask for the one it minted.
    const KEY_SITES: [&str; 5] = [
        ".github/install-toolchain.sh",
        ".github/workflows/ci.yml",
        ".github/workflows/gate-a.yml",
        ".github/workflows/probe-green.yml",
        ".github/workflows/toolchain.yml",
    ];

    /// The `HEAD:<path>` list of every `git rev-parse … | sha256sum` in `text`.
    ///
    /// **The spelling this closes** is the pipe written within 300 characters of
    /// the `rev-parse`, which is every copy the tree has; a `rev-parse HEAD:`
    /// used for anything else — `toolchain.yml`'s manifest reads `HEAD:rust` on
    /// its own — is not a key and is not matched.
    fn key_paths(text: &str) -> Vec<Vec<&str>> {
        let mut out = Vec::new();
        for (at, _) in text.match_indices("git rev-parse HEAD:") {
            let rest = &text[at..];
            let window = &rest[..rest.len().min(300)];
            let Some(end) = window.find("sha256sum") else { continue };
            out.push(
                window[..end].split_whitespace().filter_map(|t| t.strip_prefix("HEAD:")).collect(),
            );
        }
        out
    }

    /// **A copy of the key that names other trees asks for a tag nothing
    /// published**, and every job in CI installs its toolchain by that tag. The
    /// expression cannot live in one place — `install-toolchain.sh` runs before
    /// there is anything to read it from — so the copies are held here.
    #[test]
    fn every_toolchain_key_names_the_same_trees() {
        let root = repo_root();
        let publisher = std::fs::read_to_string(root.join(".github/workflows/toolchain.yml"))
            .expect("toolchain.yml is what mints the tag and is not readable");
        let minted = key_paths(&publisher);
        assert_eq!(minted.len(), 1, "toolchain.yml computes the tag once: {minted:?}");
        let want = &minted[0];
        assert!(
            want.contains(&"toyos-ld/src") && want.contains(&".github/workflows/toolchain.yml"),
            "the tag is the hash of everything the tarball's bytes depend on, and this names \
             neither the linker nor the packaging: {want:?}"
        );

        for site in KEY_SITES {
            let text = std::fs::read_to_string(root.join(site))
                .unwrap_or_else(|e| panic!("{site} computes the tag and is not readable: {e}"));
            let found = key_paths(&text);
            assert!(!found.is_empty(), "{site} is on the list and computes no tag");
            for paths in found {
                assert_eq!(&paths, want, "{site} names other trees than toolchain.yml does");
            }
        }

        // A copy elsewhere in `.github` is the same drift, so the list has to be
        // the whole of it.
        let mut elsewhere = Vec::new();
        let mut stack = vec![root.join(".github")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let rel = path.strip_prefix(&root).unwrap().to_string_lossy().to_string();
                if !key_paths(&std::fs::read_to_string(&path).unwrap_or_default()).is_empty()
                    && !KEY_SITES.contains(&rel.as_str())
                {
                    elsewhere.push(rel);
                }
            }
        }
        assert!(elsewhere.is_empty(), "these compute the tag and are not on KEY_SITES: {elsewhere:?}");
    }

    #[test]
    fn the_version_parser_takes_what_qemu_prints_and_refuses_the_rest() {
        assert_eq!(
            parse_qemu_version("QEMU emulator version 11.0.3 (Debian 1:11.0.3+ds-1)\n").as_deref(),
            Some("11.0.3")
        );
        assert_eq!(
            parse_qemu_version("QEMU emulator version 8.2.2 (Debian 1:8.2.2+ds-0ubuntu1.11)\n")
                .as_deref(),
            Some("8.2.2")
        );
        assert_eq!(parse_qemu_version("qemu-system-x86_64: no such option\n"), None);
        assert_eq!(parse_qemu_version(""), None);
    }
}
