//! The harness's own checks, none of which boots a guest: `tests/toyos.rs`
//! included under the libtest harness, so every path in it resolves here, and
//! the checks beside it in a module only this target compiles.

include!("toyos.rs");

mod checks {
    use super::*;

    #[path = "audio.rs"]
    mod audio_checks;
    #[path = "clock.rs"]
    mod clock_checks;
    #[path = "metal.rs"]
    mod metal_checks;
    #[path = "qemu.rs"]
    mod qemu_checks;
    #[path = "screen.rs"]
    mod screen_checks;
    #[path = "serial.rs"]
    mod serial_checks;

    /// One subject: what a console line says died, what a wait does about it,
    /// and that only one place in the harness answers either.
    #[test]
    fn serial_vocabulary() -> Result<(), String> {
        serial_checks::self_check()?;
        qemu_checks::ceiling_self_check()?;
        qemu_checks::host_scale_self_check()?;
        one_vocabulary()
    }

    /// A wait that hands a death spelling to a scan of its own, asked of the
    /// harness's source.
    ///
    /// **The one place the vocabulary lives is the whole of the fix, so this is
    /// what keeps it the one place.**
    ///
    /// The way that comes back is the obvious patch: one more spelling handed
    /// straight to a `contains` beside the call. It would match a *program's* panic
    /// as readily as the kernel's and take the run down with a guest binary that
    /// was expected to die — so the shape is refused by name rather than left to a
    /// reviewer. Comment lines go first: this file argues about these words at
    /// length, and prose is not a second answer.
    fn hand_rolled_deaths(text: &str) -> Vec<String> {
        let mut found = Vec::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for word in serial_checks::spellings() {
                // The shape is the spelling as somebody's first argument —
                // `contains`, `starts_with`, `find`, any of them. A spelling
                // *inside* a longer staged line is how this file's own gates build
                // their inputs, and those are not scans.
                if line.contains(&format!("(\"{word}")) {
                    found.push(format!("{}:{}: {}", n + 1, word, line.trim()));
                }
            }
        }
        found
    }

    /// [`hand_rolled_deaths`] over the file that has to stay clean, with its own
    /// bad input beside it so a check that stopped finding anything says so.
    fn one_vocabulary() -> Result<(), String> {
        const FILE: &str = "tests/common/qemu.rs";
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(FILE);
        let text = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let found = hand_rolled_deaths(&text);
        if !found.is_empty() {
            return Err(format!(
                "{FILE} scans for a death spelling itself, and `serial::died` is where that is \
                 decided for every wait at once — a second answer here is what let a kernel panic \
                 read as a stall, and it matches a program's own panic besides:\n  {}",
                found.join("\n  ")
            ));
        }
        // The negative control. Every line of it is a shape this must name, and the
        // last two are shapes it must not: prose, and a staged capture built out of
        // the same words.
        let staged = "\
            } else if line.contains(\"KERNEL PANIC\") {\n\
            if line.starts_with(\"SEGFAULT\") {\n\
            // ends on `PANIC:` and nothing else, which is the defect\n\
            const KERNEL: &str = \"[kernel 1.450 cpu3] PANIC: panicked at reserve.rs:812:9:\";\n";
        let named = hand_rolled_deaths(staged);
        if named.len() != 2 {
            return Err(format!(
                "the check names {} of the two hand-rolled scans staged for it: {named:?}",
                named.len()
            ));
        }
        eprintln!("  [vocabulary] {FILE} asks `serial::died` and nothing else");
        Ok(())
    }

    #[test]
    fn suspend_detector() -> Result<(), String> {
        clock_checks::self_check()
    }

    /// What a suspend is worth to a verdict, staged rather than reasoned about.
    ///
    /// `clock_checks::self_check` gates the detector; this gates what the suite
    /// does with what it detects. Both halves are needed and neither implies the
    /// other: **a suspend that silently passes is as bad as one that silently
    /// fails**, and here the two are one line apart.
    #[test]
    fn suspend_invalidates_a_verdict() -> Result<(), String> {
        let slept = common::clock::SUSPENDED_AT_LEAST + Duration::from_secs(120);
        let awake = Duration::ZERO;
        // Under the threshold on purpose: two clock reads jitter against each other
        // by microseconds, and a run must not be thrown away for that.
        let jitter = common::clock::SUSPENDED_AT_LEAST
            .checked_sub(Duration::from_millis(1))
            .expect("SUSPENDED_AT_LEAST must be at least 1ms for this case to mean anything");
        let cases: [(&str, Option<&str>, Duration, Verdict); 6] = [
            ("a pass on a host that stayed up", None, awake, Verdict::Pass),
            ("a fail on a host that stayed up", Some("the guest said no"), awake, Verdict::Fail),
            ("a pass across a suspend", None, slept, Verdict::Invalid),
            ("a fail across a suspend", Some("timed out"), slept, Verdict::Invalid),
            ("a pass across clock jitter", None, jitter, Verdict::Pass),
            ("a fail across clock jitter", Some("the guest said no"), jitter, Verdict::Fail),
        ];
        for (what, reason, suspended, want) in cases {
            let outcome = Outcome {
                name: what.to_string(),
                reason: reason.map(str::to_string),
                elapsed: Duration::from_secs(3),
                suspended,
            };
            let got = outcome.verdict();
            if got != want {
                return Err(format!("{what} is {got:?}, and it has to be {want:?}"));
            }
        }
        Ok(())
    }

    /// A blown ceiling stays red, and is named apart from a failed assertion.
    ///
    /// Both halves, because each fails the other's way round. An implementation
    /// that made a stall its own non-red status would hide a guest that genuinely
    /// stops; one that only renamed the line would leave the summary saying a test
    /// found something. Staged against the strings a wait actually produces rather
    /// than against the marker on its own, because a caller prefixes its own
    /// sentence to [`await_marker`]'s and the classification has to survive that.
    #[test]
    fn a_stall_stays_red() -> Result<(), String> {
        // Built from the marker rather than copied, so a rename cannot leave the
        // gate asserting against a string nothing produces any more.
        let real = format!("{STALLED} waiting for the long tone to start — it went quiet");
        let under_a_sentence = format!("the compositor stopped painting\n{real}");
        let past = qemu::GUEST_WEDGED + Duration::from_secs(1);
        let backstop = qemu::ceiling_verdict(None, past, qemu::GUEST_WEDGED, Duration::from_secs(1), 900)
            .ok_or("a guest talking past the backstop was given no verdict")?;
        let cases: [(&str, Option<&str>, bool); 5] = [
            ("an ordinary red", Some("the pointer never moved right"), false),
            ("a wait that expired", Some(real.as_str()), true),
            (
                "a wait that expired under a caller's own sentence",
                Some(under_a_sentence.as_str()),
                true,
            ),
            ("the backstop on a guest still talking", Some(backstop.as_str()), true),
            ("a pass", None, false),
        ];
        for (what, reason, want_stall) in cases {
            let outcome = Outcome {
                name: what.to_string(),
                reason: reason.map(str::to_string),
                elapsed: Duration::from_secs(1),
                suspended: Duration::ZERO,
            };
            if outcome.stalled() != want_stall {
                return Err(format!(
                    "{what} reads as stalled={}, and it has to be {want_stall}",
                    outcome.stalled()
                ));
            }
            // Red is red. A stall that stopped failing the run would be a gate that
            // reports and enforces nothing.
            let red = outcome.verdict() == Verdict::Fail;
            if red != reason.is_some() {
                return Err(format!("{what} is red={red}, and a reason is always red"));
            }
        }

        let mut tally = Tally::new();
        tally.record(Outcome {
            name: "a_stalled_test".to_string(),
            reason: Some(format!("{STALLED} waiting for nothing at all — it went quiet")),
            elapsed: Duration::from_secs(1),
            suspended: Duration::ZERO,
        });
        tally.record(Outcome {
            name: "a_wrong_answer".to_string(),
            reason: Some("the pointer never moved right".to_string()),
            elapsed: Duration::from_secs(1),
            suspended: Duration::ZERO,
        });
        if tally.exit_code() != 1 {
            return Err(format!("two reds exited {}, and they have to red", tally.exit_code()));
        }
        if tally.stalls != ["a_stalled_test"] {
            return Err(format!(
                "the run named {:?} as blown guards; it has to name exactly the one that was",
                tally.stalls
            ));
        }
        let summary = tally.summary(2, Duration::from_secs(2), Duration::ZERO);
        if !summary.contains("1 of those reds are the ceiling") {
            return Err(format!("the summary does not separate the two kinds of red:\n{summary}"));
        }
        Ok(())
    }

    /// One live guest holds its lane's NVMe image, and the next one may not.
    ///
    /// The ordering itself is now the type's: `boot` takes a [`qemu::LaneFree`] and
    /// the only thing that makes one out of a guest is `QemuInstance::shutdown`,
    /// which takes it by value. What is left to check at runtime is the claim
    /// underneath — that a hold is real while a guest is up and gone once it is
    /// not — and this checks it on the harness's own registry, in both directions,
    /// with no guest.
    #[test]
    fn nvme_image_is_held_by_one_guest() -> Result<(), String> {
        // Names, not files: a claim is a hold on a path and touches no disk, so
        // nothing here has to create or delete a hundred megabytes to ask.
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let image = dir.join("nvme-claim-gate.img");
        let other = dir.join("nvme-claim-gate-other.img");

        let held = qemu::NvmeClaim::take(&image).map_err(|why| {
            format!("a free image refused its first guest: {why}")
        })?;

        // The overlap. This is the direction that must red, and it is what the
        // reboot produced.
        match qemu::NvmeClaim::take(&image) {
            Ok(_) => {
                return Err(format!(
                    "a second guest took {}, which a live one is holding — two QEMUs are then \
                     handed one image and the second dies on its lock",
                    image.display()
                ))
            }
            Err(why) => {
                // The refusal has to name the image, or it cannot be acted on: a
                // run makes dozens of guests and the message is all a reader gets.
                if !why.contains(&image.display().to_string()) {
                    return Err(format!("the refusal does not name the image it is about: {why}"));
                }
            }
        }

        // A different image is not a conflict, or every lane would refuse every
        // other lane's boot the moment this gate had teeth.
        let elsewhere = qemu::NvmeClaim::take(&other)
            .map_err(|why| format!("an unheld image was refused: {why}"))?;
        drop(elsewhere);

        // And the ordinary reboot: the replacement takes the image the guest it
        // replaces released. Green, and it is the half a fix that simply refused
        // every second boot would break.
        drop(held);
        let replacement = qemu::NvmeClaim::take(&image).map_err(|why| {
            format!("a replacement was refused the image its predecessor released: {why}")
        })?;
        drop(replacement);
        Ok(())
    }

    /// [`control_regs`] against machines this host cannot boot, with no guest.
    ///
    /// [`control_regs_negative`] runs the real defective machine and is the link
    /// between this verdict and a kernel; what is here is the states no actuator
    /// reaches — a CPU that differs from three others, a bit set uniformly on all
    /// four, an AP that never printed. Every value is one this tree has printed or
    /// one bit away from it.
    #[test]
    fn control_regs_verdict() -> Result<(), String> {
        const AP_BEFORE: (u64, u64) = (0xe000_0011, 0x0031_0620);
        const DECLARED: (u64, u64) = (0x8001_0033, 0x0030_0668);

        fn log(cpus: &[(u64, u64)]) -> String {
            cpus.iter()
                .enumerate()
                .map(|(i, (cr0, cr4))| {
                    format!("[kernel 0.1 cpu{i}] control_regs: cpu{i} cr0={cr0:#010x} cr4={cr4:#010x}\n")
                })
                .collect()
        }

        let refused = |what: &str, cpus: &[(u64, u64)], says: &str| match control_regs(&log(cpus), 4) {
            Ok(()) => Err(format!("{what} was accepted")),
            Err(e) if e.contains(says) => Ok(()),
            Err(e) => Err(format!("{what} was refused for the wrong reason: {e}")),
        };

        // Positive control first: a verdict that refuses everything refuses the
        // defect too, and would prove nothing below.
        control_regs(&log(&[DECLARED; 4]), 4)
            .map_err(|e| format!("the declared machine was refused: {e}"))?;

        refused("the machine this tree booted", &[DECLARED, AP_BEFORE, AP_BEFORE, AP_BEFORE], "CD")?;
        // The case a "do all the CPUs agree?" test passes: they agree, on INIT's
        // value. Nothing about uniformity says caching is on.
        refused("four CPUs agreeing on INIT's CR0", &[AP_BEFORE; 4], "CD")?;
        refused(
            "one CPU without WP",
            &[DECLARED, (DECLARED.0 & !(1 << 16), DECLARED.1), DECLARED, DECLARED],
            "WP",
        )?;
        refused(
            "one CPU without NE",
            &[DECLARED, DECLARED, (DECLARED.0 & !(1 << 5), DECLARED.1), DECLARED],
            "NE",
        )?;
        // The bit that must be *absent*: with it set, XCR0 can name components
        // FXSAVE64 does not save.
        refused("OSXSAVE set", &[(DECLARED.0, DECLARED.1 | (1 << 18)); 4], "OSXSAVE")?;
        // Two bits a machine could hold uniformly, each one line of kernel diff
        // away, and neither reachable by an actuator. `AM` is named clear above and
        // answers by name; `PGE` is named nowhere, which is the case the whole
        // never-named rule exists for — `TSD` and `PKE` are the same case.
        refused("every CPU with AM set", &[(DECLARED.0 | (1 << 18), DECLARED.1); 4], "AM")?;
        refused(
            "every CPU with PGE set",
            &[(DECLARED.0, DECLARED.1 | (1 << 7)); 4],
            "never named",
        )?;
        refused("every CPU without SMEP", &[(DECLARED.0, DECLARED.1 & !(1 << 20)); 4], "SMEP")?;
        // A CPU that agrees about every named bit and differs in one the CPU is
        // allowed to withhold, so nothing above it can object.
        refused("one CPU with PCID and three without", &[DECLARED, DECLARED, DECLARED, (DECLARED.0, DECLARED.1 | (1 << 17))], "cpu3")?;
        // And an AP that never printed at all, which is what a machine whose AP
        // died before the check looks like.
        refused("three lines for four CPUs", &[DECLARED; 3], "{0, 1, 2, 3}")?;

        eprintln!("  [control_regs] the verdict refuses 10 machines and accepts the declared one");
        Ok(())
    }

    /// What the declaration itself has to be, before any of it means anything.
    /// Which shared-boot binaries need `SYS_DEBUG`, asked of their source.
    ///
    /// A name reaches the syscall directly, or through a child it spawns.
    fn needs_actuators(sources: &[(String, String)], registry: &[&str]) -> BTreeSet<String> {
        // The fourth spelling is the argument-taking form: every action that
        // carries a payload (TLB_ACK_DELAY_ARM, CENSUS_KIND, LOWER_SYSINFO_BOUND,
        // SLOT_TO_LAST_GENERATION) is reached through `debug_with`, never
        // `debug`. The third is the SDK's: `toyos::census` calls `debug_with` on
        // the caller's behalf, so a binary whose leak assertion is a census names
        // no syscall of its own and reads as innocent to the others.
        let calls = |text: &str| {
            text.contains("SYS_DEBUG")
                || text.contains("syscall::debug(")
                || text.contains("syscall::debug_with(")
                || text.contains("census::Census")
        };
        let direct: BTreeSet<&str> =
            sources.iter().filter(|(_, t)| calls(t)).map(|(n, _)| n.as_str()).collect();
        let mut out = BTreeSet::new();
        for (name, text) in sources {
            if !registry.contains(&name.as_str()) {
                continue;
            }
            let spawns = direct.iter().any(|d| text.contains(&format!("test_rs_{d}")));
            if direct.contains(name.as_str()) || spawns {
                out.insert(name.clone());
            }
        }
        out
    }

    /// [`ACTUATOR_TESTS`] is exactly the shared-boot binaries that reach
    /// `SYS_DEBUG`, and the binaries are what is asked.
    ///
    /// **What this does not cover, stated because the hole is real:** a machine or
    /// screen test that *drives* one of those binaries on a boot of its own. No
    /// static rule here can say which `BootOptions` a `run_test` call belongs to.
    /// What answers it instead is the guest: `test_panic_child` names
    /// `InvalidArgument` as *this kernel carries no actuators* rather than reporting
    /// a kernel that failed to stop, so the red says what is wrong wherever it
    /// happens.
    ///
    /// **Both directions are the point.** A binary that gains a `debug()` call and
    /// no entry would run on the shipping kernel, where the syscall answers
    /// `InvalidArgument` — and a test whose verdict is that a process died would
    /// then fail for a reason with nothing to do with what it is about. An entry
    /// whose binary no longer calls it is a test kept off the shipping kernel for
    /// nothing, which is the erosion this split exists to stop.
    #[test]
    fn suite_split() -> Result<(), String> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/toyos-rust-tests/src/bin");
        let mut sources: Vec<(String, String)> = Vec::new();
        for entry in fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let text = fs::read_to_string(&path).map_err(|e| format!("read {name}: {e}"))?;
            sources.push((name, text));
        }
        let registry: Vec<&str> =
            sources.iter().map(|(n, _)| n.as_str()).filter(|n| !RUST_SKIP.contains(n)).collect();

        // The negative control, and it carries its own bad input: a binary that
        // calls the syscall and is on no list must be named, or the check above is
        // a spelling of `true`.
        let staged = vec![
            ("a_listed_one".to_string(), "syscall::debug(3)".to_string()),
            ("an_unlisted_one".to_string(), "SYS_DEBUG".to_string()),
            ("its_parent".to_string(), "Command::new(\"/system/bin/test_rs_an_unlisted_one\")".to_string()),
            ("a_censor".to_string(), "use toyos::census::Census;".to_string()),
            ("a_debug_with_user".to_string(), "syscall::debug_with(3, 4)".to_string()),
            ("innocent".to_string(), "println!()".to_string()),
        ];
        let staged_registry = [
            "a_listed_one",
            "an_unlisted_one",
            "its_parent",
            "a_censor",
            "a_debug_with_user",
            "innocent",
        ];
        let found = needs_actuators(&staged, &staged_registry);
        let want: BTreeSet<String> =
            ["a_listed_one", "an_unlisted_one", "its_parent", "a_censor", "a_debug_with_user"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        if found != want {
            return Err(format!("the check does not work: on staged input it named {found:?}"));
        }

        let want: BTreeSet<String> = needs_actuators(&sources, &registry);
        let listed: BTreeSet<String> = ACTUATOR_TESTS.iter().map(|s| s.to_string()).collect();
        let missing: Vec<&String> = want.difference(&listed).collect();
        if !missing.is_empty() {
            return Err(format!(
                "{missing:?} reach SYS_DEBUG and are on the shipping boot, where the syscall answers \
                 InvalidArgument. Add each to ACTUATOR_TESTS, or to RUST_SKIP if it is driven rather \
                 than run."
            ));
        }
        let stale: Vec<&String> = listed.difference(&want).collect();
        if !stale.is_empty() {
            return Err(format!(
                "{stale:?} are held off the shipping kernel and no longer reach SYS_DEBUG. Delete \
                 each entry — coverage of the binary an image ships is what it costs."
            ));
        }
        // **The other shape [`registered`] cannot see**: a binary a machine
        // test drives under a *different* name is still discovered here, still runs
        // on the shared boot, and there passes on its exit code with nothing staged
        // for it to act on.
        let staged_driven = [
            String::from("qemu.run_test(\"test_rs_a_driven_one\", Duration::from_secs(30))"),
            String::from("Command::new(\"/system/bin/test_rs_another_driven\")"),
            String::from("qemu.run_test(&format!(\"test_rs_{name}\"), ceiling)"),
        ];
        let found = driven_binaries(&staged_driven);
        let want: BTreeSet<String> =
            ["a_driven_one", "another_driven"].iter().map(|s| s.to_string()).collect();
        if found != want {
            return Err(format!("the driven-name reader does not work: it named {found:?}"));
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut harness = vec![fs::read_to_string(root.join("tests/toyos.rs"))
            .map_err(|e| format!("read tests/toyos.rs: {e}"))?];
        let common = root.join("tests/common");
        for entry in fs::read_dir(&common).map_err(|e| format!("read {}: {e}", common.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|e| e == "rs") {
                harness.push(fs::read_to_string(&path).map_err(|e| e.to_string())?);
            }
        }
        let shared: BTreeSet<&str> = registry.iter().copied().collect();
        let both: BTreeSet<String> = driven_binaries(&harness)
            .into_iter()
            .filter(|name| shared.contains(name.as_str()))
            .collect();
        let declared: BTreeSet<String> = DRIVEN_AND_SHARED.iter().map(|s| s.to_string()).collect();
        let undeclared: Vec<&String> = both.difference(&declared).collect();
        if !undeclared.is_empty() {
            return Err(format!(
                "{undeclared:?} are driven by a machine test and also run on the shared boot, where \
                 nothing stages what they need — so each passes on its exit code with no verdict. Add \
                 each to RUST_SKIP with the reason its driver exists, or to DRIVEN_AND_SHARED if its \
                 shared run asserts something of its own."
            ));
        }
        let stale: Vec<&String> = declared.difference(&both).collect();
        if !stale.is_empty() {
            return Err(format!(
                "DRIVEN_AND_SHARED names {stale:?}, which no machine test drives or the shared boot \
                 no longer runs. Delete each entry — a declaration nothing is true of is what makes \
                 the rest of the list unreadable."
            ));
        }

        println!(
            "  [split] {} shared binaries on the shipping kernel, {} on the actuator one, {} of them \
             driven elsewhere and declared",
            registry.len() - listed.len(),
            listed.len(),
            both.len()
        );
        Ok(())
    }

    /// Every guest binary the harness drives by name, read out of its own sources.
    ///
    /// A driver reaches a binary as the literal `test_rs_<name>`, so that is what
    /// says a binary has one; a `format!` over a variable name yields no literal
    /// and is not a driver of any particular binary. Pure, and the sources are a
    /// parameter, so `suite_split` stages its own before trusting it on the tree.
    fn driven_binaries(sources: &[String]) -> BTreeSet<String> {
        const MARK: &str = "test_rs_";
        let mut found = BTreeSet::new();
        for text in sources {
            let mut rest = text.as_str();
            while let Some(at) = rest.find(MARK) {
                rest = &rest[at + MARK.len()..];
                let end = rest
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .unwrap_or(rest.len());
                if end > 0 {
                    found.insert(rest[..end].to_string());
                }
            }
        }
        found
    }

    /// What a whole run exits with, and what its last line says.
    #[test]
    fn run_exit_status() -> Result<(), String> {
        let outcome = |name: &str, reason: Option<&str>, suspended: Duration| Outcome {
            name: name.to_string(),
            reason: reason.map(str::to_string),
            elapsed: Duration::from_secs(3),
            suspended,
        };
        let slept = common::clock::SUSPENDED_AT_LEAST + Duration::from_secs(120);

        let mut red = Tally::new();
        red.record(outcome("a_red", Some("the disk came back short"), Duration::ZERO));
        red.record(outcome("a_suspended_one", None, slept));
        if red.exit_code() != 1 {
            return Err(format!("a run with a red exits {}, and it has to be 1", red.exit_code()));
        }

        let mut suspended = Tally::new();
        suspended.record(outcome("a_suspended_one", None, slept));
        if suspended.exit_code() != 2 {
            return Err(format!("a suspended run exits {}, and it has to be 2", suspended.exit_code()));
        }

        // The clean case, so that none of the above is passing because everything
        // reds.
        let mut clean = Tally::new();
        clean.record(outcome("a_green", None, Duration::ZERO));
        let text = clean.summary(1, Duration::from_secs(9), Duration::ZERO);
        if clean.exit_code() != 0 {
            return Err(format!("a clean run exits {}, and it has to be 0", clean.exit_code()));
        }
        if !text.lines().last().unwrap_or_default().starts_with("test result: ok.") {
            return Err(format!("a clean run does not say so plainly:\n{text}"));
        }
        Ok(())
    }

    fn shared_row(name: &str) -> TestDef {
        TestDef {
            name: name.to_string(),
            qemu_name: format!("test_rs_{name}"),
            timeout: Duration::from_secs(1),
            check: |_| true,
            settle: no_settle,
        }
    }

    /// A run takes every registered test its filter matches, and a shard drops
    /// exactly the screen rows whose profile is not of [`toyos_build::ci::GUEST_ARCH`],
    /// saying which.
    #[test]
    fn a_run_selects_by_filter_and_shard() -> Result<(), String> {
        let shared = [shared_row("shared_one")];
        let taken = |filter: Option<&str>, sharded: bool| -> BTreeSet<String> {
            let (tests, machine, screen) = select(&shared, filter, sharded);
            tests
                .iter()
                .map(|t| t.name.clone())
                .chain(machine.iter().map(|(n, _)| n.to_string()))
                .chain(screen.iter().map(|(n, _, _)| n.to_string()))
                .collect()
        };
        let names = |of: &[&str]| -> BTreeSet<String> { of.iter().map(|n| n.to_string()).collect() };
        let enabled = |n: &&str| redlist::disabled(redlist::DISABLED, n).is_none();
        let every: BTreeSet<String> =
            declared().chain(["shared_one"]).filter(enabled).map(String::from).collect();
        let foreign: BTreeSet<String> = SCREEN_TESTS
            .iter()
            .filter(|(_, _, profile)| profile.arch() != toyos_build::ci::GUEST_ARCH)
            .map(|(n, _, _)| *n)
            .filter(enabled)
            .map(String::from)
            .collect();
        if !foreign.contains("virt_el2_drop") {
            return Err(format!("the premise: virt_el2_drop is a guest no CI lane boots, and {foreign:?} lacks it"));
        }
        let cases = [
            (None, false, every.clone()),
            (None, true, every.difference(&foreign).cloned().collect()),
            (Some("virt_el2"), false, names(&["virt_el2_drop"])),
            (Some("el2_drop"), false, names(&["virt_el2_drop"])),
            (Some("virt_el2"), true, BTreeSet::new()),
            (Some("sshd_"), true, names(&["sshd_exec", "sshd_files", "sshd_key_auth"])),
            (Some("shared_one"), true, names(&["shared_one"])),
        ];
        for (filter, sharded, want) in cases {
            let got = taken(filter, sharded);
            if got != want {
                return Err(format!(
                    "filter {filter:?}, sharded {sharded}: took {:?} it should not and left out {:?}",
                    got.difference(&want).collect::<Vec<_>>(),
                    want.difference(&got).collect::<Vec<_>>()
                ));
            }
        }
        let named = |filter: Option<&str>| -> Option<(String, BTreeSet<String>)> {
            let line = arch_drop_line(&shared, filter)?;
            let (_, rows) = line.rsplit_once(": ").expect("the line names its rows after a colon");
            let rows = rows.split(", ").map(String::from).collect();
            Some((line, rows))
        };
        let (line, rows) =
            named(None).ok_or("a shard that drops the rows of another architecture said nothing")?;
        if rows != foreign || !line.starts_with(&format!("{} test(s)", foreign.len())) {
            return Err(format!("a shard dropping {foreign:?} said {line:?}"));
        }
        let named_el2 = named(Some("el2_drop")).map(|(_, rows)| rows);
        if named_el2 != Some(names(&["virt_el2_drop"])) {
            return Err(format!("filter el2_drop: a shard said {named_el2:?}"));
        }
        if let Some((line, _)) = named(Some("sshd_")) {
            return Err(format!("a filter matching no foreign row still had a shard say {line:?}"));
        }
        Ok(())
    }

    /// Two rows under one name are refused, whether both are shared-boot rows
    /// or one is a declared registry's.
    #[test]
    fn a_name_registered_twice_is_refused() -> Result<(), String> {
        let apart = [shared_row("shared_one"), shared_row("shared_two")];
        let names = registered(&apart)?;
        for name in ["shared_one", "shared_two", "virt_el2_drop"] {
            if !names.contains(name) {
                return Err(format!("{name} is not among the {} registered names", names.len()));
            }
        }
        for twice in [
            [shared_row("shared_one"), shared_row("shared_one")],
            [shared_row("shared_one"), shared_row("virt_el2_drop")],
        ] {
            let twice_name = &twice[1].name;
            match registered(&twice) {
                Err(refusal) if refusal.contains(&format!("{twice_name} is registered twice")) => {}
                other => {
                    let answer = other.map(|names| names.len());
                    return Err(format!("{twice_name} registered twice was answered {answer:?}"));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn screen_decoder() {
        screen_checks::self_test();
    }

    #[test]
    fn firmware_rows_at_the_edge() -> Result<(), String> {
        screen_checks::edge_self_test()
    }

    #[test]
    fn metal_audio_judges() -> Result<(), String> {
        audio_checks::judges_verdict()
    }

    #[test]
    fn metal_stop_owes_its_record_and_leaves_no_operation_open() {
        metal_checks::the_stop_owes_its_record_and_leaves_no_operation_open();
    }

    #[test]
    fn metal_bound_fires_within_one_period_of_itself() {
        metal_checks::a_bound_fires_within_one_period_of_itself();
    }

    #[test]
    fn metal_name_measured_twice_is_refused() {
        metal_checks::a_name_measured_twice_is_refused();
    }

    #[test]
    fn metal_boot_with_a_failure_of_its_own_adds_no_row() {
        metal_checks::a_boot_with_a_failure_of_its_own_adds_no_row();
    }

    #[test]
    fn metal_name_two_boots_measured_is_refused() {
        metal_checks::a_name_two_boots_measured_is_refused();
    }

    #[test]
    fn metal_reading_past_its_record_fails_and_moves_nothing() {
        metal_checks::a_reading_past_its_record_fails_and_moves_nothing();
    }

    #[test]
    fn metal_run_under_another_bios_fails_and_records_nothing() {
        metal_checks::a_run_under_another_bios_fails_and_records_nothing();
    }

    #[test]
    fn metal_failing_shared_member_fails_its_boot() {
        metal_checks::a_failing_shared_member_fails_its_boot();
    }

    #[test]
    fn metal_cleared_page_owes_no_panel() {
        metal_checks::a_cleared_page_owes_no_panel();
    }

    #[test]
    fn metal_list_from_parse_reaches_run_without_the_machine() -> Result<(), String> {
        let args: Vec<String> = ["--metal", "--list"].iter().map(ToString::to_string).collect();
        let mode = testargs::parse(&args)?
            .metal
            .ok_or_else(|| "--metal --list resolved to no mode at all".to_string())?;
        if mode != testargs::MetalMode::List {
            return Err(format!("--metal --list resolved to {mode:?}"));
        }
        let selected: Vec<(&str, &'static metal::Metal)> = METAL
            .iter()
            .find(|(name, _)| *name == "blackbox_unclaimed_page")
            .map(|(name, decl)| vec![(*name, decl)])
            .ok_or_else(|| "blackbox_unclaimed_page is not registered".to_string())?;
        let verdict = metal::run(mode, &selected, &[], &[], true);
        if verdict != metal::Verdict::Green {
            return Err(format!("--metal --list produced {verdict:?}, not Green"));
        }
        Ok(())
    }

    /// The judge a metal registration runs.
    fn metal_judge(name: &str) -> fn(&[&metal::Readback]) -> Result<(), String> {
        match METAL.iter().find(|(row, _)| *row == name) {
            Some((_, metal::Metal::Runs { judge, .. })) => *judge,
            _ => panic!("{name} runs no metal judge"),
        }
    }

    /// One boot's readback, out of a `loader.log` and a `logd` text.
    fn readback(label: &str, loader: &str, log: &str) -> metal::Readback {
        let boot = "back_secs 50\nstick_secs 0\n";
        metal::Readback::new(label, std::path::PathBuf::new(), loader.into(), log.into(), boot)
            .expect("a boot file naming both numbers")
    }

    /// The pass before the handoff, which every `loader.log` opens with.
    const HANDOFF: &str = "Loader log: the kernel handoff begins, so this file ends here\n";

    /// Four judges fed a T14 readback's lines: each record they ask for is
    /// written after the file was made whole, so it crosses only on the sealed
    /// page — and a page without it still reds.
    #[test]
    fn metal_judges_read_the_page_for_what_only_the_page_carries() {
        let done = |tail: &str| {
            format!(
                "{HANDOFF}{}\nToyOS Bootloader 1.0\n\
                 Black box: the last boot read DONE, so it handed the machine back on purpose and \
                 this chain ends here\n\
                 | log: this boot's newest records follow, newest first (16)\n{tail}\
                 Loader log: the last boot is accounted for, so this pass resets the machine\n",
                bootlog::SEPARATOR
            )
        };
        let rebooted = "| log-tail: [1.516 cpu0] Rebooting.\n";
        let stopped = "| log-tail: [1.209 cpu0] stop: 13 of 13 userland thread(s) stopped across 8 \
                       cpu(s) in 0 ms of a 2010 ms budget over 1 sweep(s), 0 of 38 userland block \
                       operation(s) still open\n";

        let jobcase =
            "[2026-09-29 10:40:36 0.000 cpu0 boot] ACPI: reset register SystemIO 0xcf9 <- 0x06\n";
        let judge = metal_judge("machine_reboot");
        assert_eq!(judge(&[&readback("jobcase", &done(rebooted), jobcase)]), Ok(()));
        assert!(judge(&[&readback("jobcase", &done(stopped), jobcase)]).is_err());

        let testcases = "[2026-09-29 11:11:32 12.720 cpu2] exit: test_rs_null_sink_client_ex pid=12 \
                         code=0 cpu=42ms\n\
                         [2026-09-29 11:11:32 12.725 cpu7] exit: echo pid=15 code=0 cpu=0ms\n";
        let judge = metal_judge("log_poll_outlives_a_close");
        assert_eq!(judge(&[&readback("testcases", &done(rebooted), testcases)]), Ok(()));
        assert!(judge(&[&readback("testcases", &done(stopped), testcases)]).is_err());

        let wedged = |tail: &str| {
            format!(
                "{HANDOFF}{}\nToyOS Bootloader 1.0\n\
                 Previous boot's panic: the last boot read WEDGED, so a bound of its own ended it \
                 and this chain ends here\n\
                 | the boot deadline expired: a bound of 120000 ms, reached at 120061 ms, with this \
                 machine in `complete`. The tail of the log ring follows ... which is what nothing \
                 was draining.\n\
                 | usb-quiesce: no barrier was taken, so this reset is not the shutdown's\n{tail}\
                 Loader log: the last boot is accounted for, so this pass resets the machine\n",
                bootlog::SEPARATOR
            )
        };
        let kernel = "[2026-09-29 10:33:28 0.000 cpu0 boot] panic console: armed 1920x1080 \
                      stride=1920 format=1 at 0x4000000000, write-combining\n";
        let wedge = "| [1.509 cpu0] wedge: staged, and only the boot deadline ends this machine: \
                     every CPU stops taking scheduler passes from here\n\
                     | [1.509 cpu0] wedge: cpu0 arrived with interrupts off, through the syscall \
                     gate, and takes them again here\n\
                     | [1.509 cpu1] wedge: cpu1 arrived with interrupts on\n";
        let judge = metal_judge("boot_deadline_ends_a_wedge");
        assert_eq!(judge(&[&readback("deadlinewedge", &wedged(wedge), kernel)]), Ok(()));
        assert!(judge(&[&readback("deadlinewedge", &wedged(""), kernel)]).is_err());

        let sweep = "| [1.526 cpu0] usb-load: sweeping disk 0 from block 6569336 to 7507812, \
                     rewriting each run with the bytes just read from it, until this machine is \
                     reset out from under it\n";
        let judge = metal_judge("usb_reset_records_the_phase_it_cut");
        assert_eq!(judge(&[&readback("usbload", &wedged(sweep), kernel)]), Ok(()));
        assert!(judge(&[&readback("usbload", &wedged(""), kernel)]).is_err());
    }

    /// The foreign-identity arm's pass after the reset: the record it cleared
    /// was sealed `DONE`, so the stop the arm staged finished.
    #[test]
    fn the_foreign_record_judge_demands_the_stop_sealed_done() {
        let loader = format!(
            "{HANDOFF}{}\nToyOS Bootloader 1.0\n\
             Black box: 0x8000000 held a DONE record another image left in this memory ([3e, d4, \
             0b, d4, 87, ad, 6a, 47, 84, b4, af, c3, f3, 6b, f7, 81], and this stick is [c1, d4, \
             0b, d4, 87, ad, 6a, 47, 84, b4, af, c3, f3, 6b, f7, 81]), armed at 2026-09-29-131341. \
             It has been cleared and this pass boots its kernel\n\
             Boot attempts: this image has had the machine 1 time(s) without reporting; now 0\n\
             {}\n\
             Loader log: the last boot is accounted for, so this pass resets the machine\n",
            bootlog::SEPARATOR,
            bootlog::HUNG_WITHOUT_A_RECORD
        );
        let kernel = "[2026-09-29 13:13:43 1.171 cpu0] Boot: complete (1171ms)\n";
        let judge = metal_judge("blackbox_foreign_record");
        assert_eq!(judge(&[&readback("foreignrecord", &loader, kernel)]), Ok(()));
        for state in ["PANIC", "WEDGED"] {
            let ended = loader.replace("held a DONE record", &format!("held a {state} record"));
            assert!(judge(&[&readback("foreignrecord", &ended, kernel)]).is_err());
            let stale = format!(
                "Black box: 0x8000000 held a DONE record another image left in this memory\n{ended}"
            );
            assert!(judge(&[&readback("foreignrecord", &stale, kernel)]).is_err());
        }
        let unbounded = loader.replace(bootlog::HUNG_WITHOUT_A_RECORD, "");
        assert!(judge(&[&readback("foreignrecord", &unbounded, kernel)]).is_err());
    }

    /// A T14 controller's handoff: it publishes USB Legacy Support and
    /// firmware never claimed it. The T14 has two, and each is judged.
    #[test]
    fn the_xecp_judge_reads_the_t14s_handoff() {
        let t14 = "[2026-09-29 11:05:25 0.253 cpu0] xHCI: xecp selftest 8/8 malformed lists refused\n\
                   [2026-09-29 11:05:25 0.253 cpu0] xHCI: firmware did not claim the controller \
                   (USBLEGSUP 0x01002201)\n\
                   [2026-09-29 11:05:25 0.253 cpu0] xHCI: USBLEGCTLSTS 0xe0000000 -> 0x00000000 \
                   (SMI generation off)\n\
                   [2026-09-29 11:05:25 0.253 cpu0] xHCI: controller reset\n\
                   [2026-09-29 11:05:25 0.254 cpu0] xHCI: controller started\n";
        assert_eq!(xhci_xecp(t14), Ok(()));
        assert_eq!(xhci_xecp(&format!("{t14}{t14}")), Ok(()));
        // Firmware that kept the controller handed nothing over.
        let held = t14.replace(
            "firmware did not claim the controller (USBLEGSUP 0x01002201)",
            "firmware still owns the controller after 1000ms (USBLEGSUP 0x01010001 -> \
             0x01010001) — resetting it anyway",
        );
        assert!(xhci_xecp(&held).is_err());
        assert!(xhci_xecp(&format!("{t14}{held}")).is_err());
        let kept = held.replace("xHCI: controller reset", "xHCI: 34 scratchpad buffers configured");
        assert!(xhci_xecp(&format!("{t14}{kept}")).is_err());
        // A second controller's handoff with no reset of its own, and a reset
        // with no handoff before it.
        let unreset = t14.replace("xHCI: controller reset", "xHCI: 34 scratchpad buffers configured");
        assert!(xhci_xecp(&format!("{t14}{unreset}")).is_err());
        assert!(xhci_xecp(&format!("{unreset}{t14}")).is_err());
        let unhanded = t14.replace("firmware did not claim the controller", "USB 3.1 on ports 2..=5");
        assert!(xhci_xecp(&format!("{t14}{unhanded}")).is_err());
        let silent = unhanded.replace("xHCI: controller reset", "xHCI: 34 scratchpad buffers configured");
        assert!(xhci_xecp(&silent).is_err());
    }

    /// `dlopen_dedup` reads `test_rs_std_tls` by path.
    #[test]
    fn a_shared_chunk_stages_every_binary_its_members_name() {
        let source = "const NEEDS_A_LIB: &str = \"/system/bin/test_rs_std_tls\";\n";
        let bins: Vec<(String, Vec<u8>)> = ["dlopen_dedup", "std_tls", "fs_large_file", "libfoo.so"]
            .iter()
            .map(|name| ((*name).to_string(), Vec::new()))
            .collect();
        let jobs = vec!["test_rs_dlopen_dedup".to_string()];
        let staged = |text: &str| -> Vec<String> {
            metal::reached(text, &jobs, &bins).into_iter().map(|(path, _)| path).collect()
        };
        assert_eq!(
            staged(&format!("test_rs_dlopen_dedup\n{source}")),
            ["bin/test_rs_std_tls", "lib/libfoo.so"]
        );
        // A longer name is another binary's.
        let longer = source.replace("test_rs_std_tls", "test_rs_std_tls_dlopen");
        assert_eq!(staged(&format!("test_rs_dlopen_dedup\n{longer}")), ["lib/libfoo.so"]);
    }

    /// `03_struct`'s output as `ccheck` prints it: the expectation the corpus
    /// stages is that, under the guest's `trim_end`.
    #[test]
    fn the_c_corpus_stages_the_expectation_the_host_compares() {
        let got = "12\n34\n12\n34\n56\n78\n~fred()";
        let boot = c_corpus_metal(&[("03_struct".to_string(), Vec::new())], |_| true);
        let staged = boot
            .files
            .iter()
            .find(|(path, _)| path == "expect/03_struct")
            .map(|(_, bytes)| String::from_utf8(bytes.clone()).expect("an expectation is text"))
            .expect("03_struct's expectation is staged");
        assert_eq!(staged.trim_end(), got);
    }
}
