//! The harness's own checks, none of which boots a guest: `tests/toyos.rs`
//! included under the libtest harness, so every path in it resolves here, and
//! the checks beside it in a module only this target compiles.

include!("toyos.rs");

mod checks {
    use super::*;
    // `toyos.rs`'s `#[macro_use]` arrives by `include!` and reaches no module
    // outside that text, so each of those names the printer's `eprintln!`.
    use toyos_build::eprintln;

    #[test]
    fn a_data_round_trip_needs_one_complete_token_per_boot() {
        let token = "0123456789abcdef0123456789abcdef";
        assert_eq!(data_roundtrip_token(&format!("[job] DATA_ROUNDTRIP written {token}\n"), "written").unwrap(), token);
        for text in [String::new(), format!("DATA_ROUNDTRIP verified {token}"), "DATA_ROUNDTRIP written 123".into(), format!("DATA_ROUNDTRIP written {token}junk"), format!("DATA_ROUNDTRIP written {token}\nDATA_ROUNDTRIP written {token}\n")] {
            assert!(data_roundtrip_token(&text, "written").is_err(), "{text}");
        }
    }

    #[path = "audio.rs"]
    mod audio_checks;
    #[path = "claims.rs"]
    mod claims_checks;
    #[path = "clock.rs"]
    mod clock_checks;
    #[path = "hostload.rs"]
    mod hostload_checks;
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
            const KERNEL: &str = \"[ 1.450 cpu3 kernel] PANIC: panicked at reserve.rs:812:9:\";\n";
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

    #[test]
    fn other_work_detector() -> Result<(), String> {
        hostload_checks::self_check()
    }

    #[test]
    fn iowait_going_back() -> Result<(), String> {
        hostload_checks::iowait_going_back()
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

    /// **A program of `libc_children` that stalls is named, and ends the
    /// row**: its boot runs nothing after it, so each later program would
    /// spend its whole guard in silence. Fed the result a guest silent past
    /// its guard gives, between a program that exits 0 and one that fails.
    #[test]
    fn a_stalled_program_ends_its_row_and_is_named() {
        let ceiling = Duration::from_secs(300);
        let stall = qemu::ceiling_verdict(None, ceiling + Duration::from_secs(16), ceiling, qemu::GUEST_QUIET, 40)
            .expect("a guest silent past its guard has a verdict");
        let window = "[ 9.100 cpu1 kernel] spawn: /system/bin/test_rs_stalls pid=31\n";
        let mut asked = Vec::new();
        let jobs: Vec<(&str, String)> =
            ["passes", "fails", "stalls", "never_run", "nor_this"].map(|name| (name, format!("test_rs_{name}"))).into();
        let verdict = common::libcchildren::judge(&jobs, |job| {
            asked.push(job.to_string());
            let (exit_code, error) = match job {
                "test_rs_passes" => (Some(0), None),
                "test_rs_fails" => (Some(3), None),
                _ => (None, Some(qemu::WaitVerdict::for_test(stall.clone(), "", window, true))),
            };
            qemu::TestResult { name: job.to_string(), exit_code, stdout: String::new(), serial: window.to_string(), error }
        });
        assert_eq!(asked, ["test_rs_passes", "test_rs_fails", "test_rs_stalls"], "a boot whose job never ended was asked for more");
        let reason = verdict.expect_err("a failed program and a stalled one are a red");
        for said in [
            "fails exited 3",
            "stalls ended with no exit code, and 2 program(s) after it were not run: never_run, nor_this",
            stall.as_str(),
            window.trim(),
        ] {
            assert!(reason.contains(said), "the row's red never says {said:?}:\n{reason}");
        }
        let outcome = Outcome { name: "libc_children".into(), reason: Some(reason), elapsed: ceiling, suspended: Duration::ZERO };
        assert!(outcome.stalled(), "the suite files the row's red as an answer and not as a blown guard");
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
        let ceiling = Duration::from_secs(30);
        let past = ceiling * 2 + Duration::from_secs(1);
        let backstop = qemu::ceiling_verdict(None, past, ceiling, Duration::from_secs(1), 900)
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

    /// `nested_nmi_is_loud`'s verdict on a whole report, on the splice CI's KVM
    /// lane recorded before the report held the console registers, on another
    /// CPU's burst inside each of the report's three lines, and on each line
    /// the kernel writes when they were not clean — held to the kernel's own
    /// words, which nothing links this crate to.
    #[test]
    fn nested_nmi_verdict() -> Result<(), String> {
        const WHOLE: &str = "[ 0.385 cpu1 kernel] CPU 1: jo\n\
             [nmi] NESTED NMI on cpu 0: a second NMI entered while IST2 was still in use.\n\
             [nmi]   rip=0xffffffff8012d3a0 rsp=0xffff80000017df50\n\
             [nmi]   the outer handler's frame is gone; the machine stops here.\n\
             ining scheduler\n\
             [ 0.390 cpu0 kernel] panic: rebooting in 60 s, timed by the calibrated clock\n";
        const SPLICED: &str = "[[kenrnmel i0.38]5  cpNu1E] CSPUT 1E: Djo inNiMngI s choednule r\n\
             c[pkeurn el0 0:.3 85a cp u1s] sechcedo: ncpud=1  rNeaMdyI=0  deyinngt=0e srtoeppded= 0 wpahrkield=0e c urrIentS=NTon2e  trwipas=s1\n \
             stil[lke rnieln 0 .3u87s cep.u1\n\
             ] [i8n04m2:i ar]me d  a t r37i2mps,= i0dlex fatf 38f7mfs,8 0 0in0te0r7rubpt3s 9\u{2014} 1th6e cp5in  hras snevper= as0sxerftefd f(kbfd 8GSI0 10, a0ux0 G0SI 612)0\n\
             df50\n\
             [nmi]   the outer handler's frame is gone; the machine stops here.\n\
             [ 0.390 cpu0 kernel] panic: rebooting in 60 s unless a key is pressed\n";
        if faults::report(WHOLE)? != WHOLE.lines().nth(1).unwrap_or_default() {
            return Err("a whole report's first line was not the one answered".into());
        }
        if faults::report(SPLICED).is_ok() {
            return Err("the recorded splice was read as a whole report".into());
        }
        // A burst of another CPU's line with no line end of its own, cut into
        // the middle of each report line in turn (on the first, behind the
        // prefix the report is found by): no line moves, so only the cut
        // line's own shape can refuse it.
        let lines: Vec<&str> = WHOLE.lines().collect();
        for at in 1..=3 {
            let (head, tail) = lines[at].split_at(lines[at].len() / 2);
            let cut = format!("{head}[ 0.386 cpu1 ker{tail}");
            let mut spliced = lines.clone();
            spliced[at] = &cut;
            if faults::report(&spliced.join("\n")).is_ok() {
                return Err(format!("a burst inside the report's line {at} was read as a whole report"));
            }
        }
        let serial = Path::new(env!("CARGO_MANIFEST_DIR")).join("kernel/src/drivers/serial.rs");
        let source = std::fs::read_to_string(&serial).map_err(|e| format!("{}: {e}", serial.display()))?;
        for said in faults::UNCLEAN {
            if !source.contains(said) {
                return Err(format!("{} writes no {said:?}, so the test refusing it refuses nothing", serial.display()));
            }
            if faults::report(&format!("\n{said}; and so on\n{WHOLE}")).is_ok() {
                return Err(format!("a capture saying {said:?} was read as clean"));
            }
        }
        Ok(())
    }

    /// `nmi-user-gs`'s judge on its two verdicts and on a boot that never
    /// reported, held to the words `kernel/src/arch/x86_64/idt/nmi_user_gs.rs`
    /// writes.
    #[test]
    fn nmi_user_gs_verdict() -> Result<(), String> {
        const PASSED: &str = "[ 1.204 cpu0 kernel] nmi-user-gs: PASS — vector 2 at CPL 0 under GS base \
             0xffff80000a3c1000: cpu0's own count 0 -> 1, 0 decoy words written, returned to GS base \
             0xffff80000a3c1000\n";
        const FAILED: &str = "[ 1.204 cpu0 kernel] nmi-user-gs: FAIL — vector 2 at CPL 0 under GS base \
             0xffff80000a3c1000: cpu0's own count 0 -> 0, 2 decoy words written, returned to GS base \
             0xffff80000a3c1000\n";
        let verdict = faults::nmi_user_gs(PASSED)?;
        if verdict != PASSED.trim_end() {
            return Err(format!("a passing boot's verdict was read as {verdict:?}"));
        }
        if faults::nmi_user_gs(FAILED).is_ok() {
            return Err("a failing verdict was read as a pass".into());
        }
        if faults::nmi_user_gs("[ 1.204 cpu0 kernel] LAPIC: spurious selftest 3/3\n").is_ok() {
            return Err("a boot that never reported was read as a pass".into());
        }
        let nmi = Path::new(env!("CARGO_MANIFEST_DIR")).join("kernel/src/arch/x86_64/idt/nmi_user_gs.rs");
        let source = std::fs::read_to_string(&nmi).map_err(|e| format!("{}: {e}", nmi.display()))?;
        for said in ["\"nmi-user-gs: {verdict} — ", "\"PASS\""] {
            if !source.contains(said) {
                return Err(format!("{} writes no {said}, so the judge reads words nothing says", nmi.display()));
            }
        }
        Ok(())
    }

    /// [`idle_second_lines`] on lines as the benches' readbacks carry them: the
    /// T14's two first queries, the kernel's first fixed-hardware interrupt, its
    /// TSC refinement and the AMD's TSC step report pass, reported;
    /// anything else stamped in the second, acpiserver's periodic count, a
    /// first time said twice and the clock's slewing of the step among it, is
    /// refused; and a line outside the second is no line of it.
    #[test]
    fn the_idle_second_admits_only_the_machines_own_lines() -> Result<(), String> {
        const QUERY_40: &str = "[2026-11-03 22:34:24 13.414 acpiserver] acpiserver: embedded controller query 0x40 taken for the first time; served by nothing";
        const QUERY_4F: &str = "[2026-11-03 22:34:25 14.039 acpiserver] acpiserver: embedded controller query 0x4f taken for the first time; served by nothing";
        const INTERRUPT: &str = "[2026-11-03 22:34:24 13.500 cpu2 kernel] isa: the ACPI fixed hardware took its first interrupt";
        const GPE: &str = "[2026-11-03 22:34:24 13.600 acpiserver] acpiserver: GPE 0x6e taken for the first time; its method ran";
        const COUNTED: &str = "[2026-11-03 22:34:24 13.700 acpiserver] acpiserver: 3 SCIs; embedded controller queries taken: 0x40 x2";
        const PROGRAM: &str = "[2026-11-03 22:34:24 13.800 test-runner pid=7] counters_metal settle: the log holds this first line";
        const BEFORE: &str = "[2026-11-03 22:34:23 12.094 cpu2 kernel] isa: a line before the second";
        const REFINED: &str = "[2026-11-04 06:25:59 13.700 cpu5 kernel] clock: over 1 s the TSC counted 2419185510351 mHz against the HPET, within 11437 ppb; the clock keeps CPUID 15H's 2419200000000 mHz";
        const STEP: &str = "[2026-11-04 06:26:03 13.750 cpu12 kernel] clock: TSC STEP: the TSC stepped 1718866 ns forward against the HPET and the PM timer; the clock slews it away at most 500 ppm, most of it in about 3437 ms";
        const SLEWED: &str = "[2026-11-04 06:26:03 13.760 cpu12 kernel] clock: the TSC's step is slewed away, 601 ns left";
        let log = |lines: &[&str]| lines.join("\n");

        let quiet = log(&[BEFORE, QUERY_40, INTERRUPT, GPE, REFINED, STEP, QUERY_4F]);
        let events = idle_second_lines(&quiet, 13_000, 14_100)
            .map_err(|rest| format!("the machine's own events were refused: {rest:?}"))?;
        if events != [QUERY_40, INTERRUPT, GPE, REFINED, STEP, QUERY_4F] {
            return Err(format!("the events reported are {events:?}"));
        }
        // A "first time" the log says twice: the second is no first take.
        const QUERY_40_AGAIN: &str = "[2026-11-03 22:34:25 13.900 acpiserver] acpiserver: embedded controller query 0x40 taken for the first time; served by nothing";
        let twice = log(&[QUERY_40, QUERY_40_AGAIN]);
        match idle_second_lines(&twice, 13_000, 14_100) {
            Err(rest) if rest == [QUERY_40, QUERY_40_AGAIN] => {}
            said => return Err(format!("a first time said twice was answered {said:?}")),
        }
        // Any other line of the clock's, its slewing of a step among them, is
        // neither a refinement nor the step's report.
        for other in [COUNTED, PROGRAM, SLEWED] {
            let busy = log(&[QUERY_40, other]);
            match idle_second_lines(&busy, 13_000, 14_100) {
                Err(rest) if rest == [other] => {}
                said => return Err(format!("{other:?} in the second was answered {said:?}")),
            }
        }
        let outside = log(&[BEFORE, PROGRAM]);
        if idle_second_lines(&outside, 13_900, 14_100) != Ok(Vec::new()) {
            return Err("a line stamped outside the second was judged inside it".into());
        }
        Ok(())
    }

    /// [`control_regs`] against machines this host cannot boot, with no guest.
    ///
    /// What is here is the states no actuator reaches — a CPU that differs from
    /// three others, a bit set uniformly on all four, an AP that never printed.
    /// Every value is one this tree has printed or one bit away from it.
    #[test]
    fn control_regs_verdict() -> Result<(), String> {
        // `xcr0=0x0` is what the kernel prints for a CPU without `CR4.OSXSAVE`.
        const AP_BEFORE: (u64, u64, u64) = (0xe000_0011, 0x0031_0620, 0);
        const DECLARED: (u64, u64, u64) = (0x8001_0033, 0x0034_06e8, 0x7);
        const AVX512: u64 = 0xe0;

        fn log(cpus: &[(u64, u64, u64)]) -> String {
            cpus.iter()
                .enumerate()
                .map(|(i, (cr0, cr4, xcr0))| {
                    format!(
                        "[ 0.100 cpu{i} kernel] control_regs: cpu{i} cr0={cr0:#010x} cr4={cr4:#010x} \
                         efer=0xd01 xcr0={xcr0:#x} save=xsaveopt smep nx\n"
                    )
                })
                .collect()
        }

        let refused = |what: &str, cpus: &[(u64, u64, u64)], says: &str| match control_regs(&log(cpus), 4) {
            Ok(()) => Err(format!("{what} was accepted")),
            Err(e) if e.contains(says) => Ok(()),
            Err(e) => Err(format!("{what} was refused for the wrong reason: {e}")),
        };

        // Positive control first: a verdict that refuses everything refuses the
        // defect too, and would prove nothing below.
        control_regs(&log(&[DECLARED; 4]), 4)
            .map_err(|e| format!("the declared machine was refused: {e}"))?;
        control_regs(&log(&[(DECLARED.0, DECLARED.1, DECLARED.2 | AVX512); 4]), 4)
            .map_err(|e| format!("the declared AVX-512 machine was refused: {e}"))?;

        refused("the machine this tree booted", &[DECLARED, AP_BEFORE, AP_BEFORE, AP_BEFORE], "CD")?;
        // The case a "do all the CPUs agree?" test passes: they agree, on INIT's
        // value. Nothing about uniformity says caching is on.
        refused("four CPUs agreeing on INIT's CR0", &[AP_BEFORE; 4], "CD")?;
        refused(
            "one CPU without WP",
            &[DECLARED, (DECLARED.0 & !(1 << 16), DECLARED.1, DECLARED.2), DECLARED, DECLARED],
            "WP",
        )?;
        refused(
            "one CPU without NE",
            &[DECLARED, DECLARED, (DECLARED.0 & !(1 << 5), DECLARED.1, DECLARED.2), DECLARED],
            "NE",
        )?;
        refused("OSXSAVE clear", &[(DECLARED.0, DECLARED.1 & !(1 << 18), 0); 4], "OSXSAVE")?;
        refused("XCR0 without AVX", &[(DECLARED.0, DECLARED.1, 0x3); 4], "xcr0.AVX")?;
        refused("XCR0 with opmask alone", &[(DECLARED.0, DECLARED.1, 0x27); 4], "part of AVX-512")?;
        // PKRU: a component this kernel's save area is not declared to hold.
        refused("XCR0 with PKRU", &[(DECLARED.0, DECLARED.1, 0x207); 4], "never named")?;
        refused(
            "one CPU with AVX-512 and three without",
            &[DECLARED, (DECLARED.0, DECLARED.1, DECLARED.2 | AVX512), DECLARED, DECLARED],
            "cpu1",
        )?;
        // Two bits a machine could hold uniformly, each one line of kernel diff
        // away, and neither reachable by an actuator. `AM` is named clear above and
        // answers by name; `PKE` is named nowhere, which is the case the whole
        // never-named rule exists for — `TSD` is the same case.
        refused("every CPU with AM set", &[(DECLARED.0 | (1 << 18), DECLARED.1, DECLARED.2); 4], "AM")?;
        refused(
            "every CPU with PKE set",
            &[(DECLARED.0, DECLARED.1 | (1 << 22), DECLARED.2); 4],
            "never named",
        )?;
        refused("every CPU without SMEP", &[(DECLARED.0, DECLARED.1 & !(1 << 20), DECLARED.2); 4], "SMEP")?;
        refused("every CPU without SMAP", &[(DECLARED.0, DECLARED.1 & !(1 << 21), DECLARED.2); 4], "SMAP")?;
        refused("every CPU without PGE", &[(DECLARED.0, DECLARED.1 & !(1 << 7), DECLARED.2); 4], "PGE")?;
        // A CPU that agrees about every named bit and differs in one the CPU is
        // allowed to withhold, so nothing above it can object.
        refused(
            "one CPU with PCID and three without",
            &[DECLARED, DECLARED, DECLARED, (DECLARED.0, DECLARED.1 | (1 << 17), DECLARED.2)],
            "cpu3",
        )?;
        // And an AP that never printed at all, which is what a machine whose AP
        // died before the check looks like.
        refused("three lines for four CPUs", &[DECLARED; 3], "{0, 1, 2, 3}")?;

        eprintln!("  [control_regs] the verdict refuses 16 machines and accepts the two declared ones");
        Ok(())
    }

    /// [`microcode_on_metal`] against both benches' machines and the ways a
    /// boot of either could fall short of what its bench is held to, with no
    /// guest.
    #[test]
    fn microcode_verdict() -> Result<(), String> {
        fn log(lines: &[(u32, &str)]) -> String {
            lines.iter().map(|(cpu, said)| format!("[ 0.050 cpu{cpu} kernel] microcode: cpu{cpu} {said}\n")).collect()
        }
        const T14_LOADER: &str =
            "[ 1.5] Microcode: /system/share/microcode/intel/06-8c-01/80, 112640 bytes at 0x7a3f0000, handed over\n";
        const AMD_LOADER: &str =
            "[ 1.5] Microcode: /system/share/microcode/amd/17-60-01, 3200 bytes at 0x7a3f0000, handed over\n";
        const T14: &str = "Intel signature 0x806c1 platform 7 found 0xbe current 0xbe running 0xbe";
        const AMD: &str = "Amd signature 0x860f01 found 0x8600106 loaded 0x860010d running 0x860010d";
        // A core's second thread, which reads the level its first loaded.
        const AMD_SIBLING: &str = "Amd signature 0x860f01 found 0x860010d loaded 0x860010d running 0x860010d";
        let t14 = |cpu: u32| (cpu, T14);
        let amd = |cpu: u32| (cpu, if cpu.is_multiple_of(2) { AMD } else { AMD_SIBLING });

        let judged = |bench: Bench, loader: &str, what: &str, lines: &[(u32, &str)], cpus: u32, says: &str| {
            match microcode_on_metal(bench, loader, &log(lines), cpus) {
                Ok(()) => Err(format!("{what} was accepted")),
                Err(e) if e.contains(says) => Ok(()),
                Err(e) => Err(format!("{what} was refused for the wrong reason: {e}")),
            }
        };
        let refused =
            |what: &str, lines: &[(u32, &str)], cpus: u32, says: &str| judged(Bench::T14, T14_LOADER, what, lines, cpus, says);
        let amd_refused = |what: &str, lines: &[(u32, &str)], says: &str| judged(Bench::Amd, AMD_LOADER, what, lines, 8, says);

        microcode_on_metal(Bench::T14, T14_LOADER, &log(&(0..8).map(t14).collect::<Vec<_>>()), 8)
            .map_err(|e| format!("the T14 at Intel's 0xbe was refused: {e}"))?;
        microcode_on_metal(Bench::Amd, AMD_LOADER, &log(&(0..8).map(amd).collect::<Vec<_>>()), 8)
            .map_err(|e| format!("the AMD bench at AMD's 0x860010d was refused: {e}"))?;

        refused("seven lines for eight CPUs", &(0..7).map(t14).collect::<Vec<_>>(), 8, "one microcode line")?;
        let mut twice: Vec<_> = (0..8).map(t14).collect();
        twice[7] = t14(6);
        refused("cpu6 twice and cpu7 never", &twice, 8, "one microcode line")?;
        let resealed: Vec<_> =
            (0..8).map(|cpu| (cpu, "Intel signature 0x806c1 platform 7 found 0xbe refused 0xbf running 0xbe")).collect();
        refused("a T14 whose CPUs refused a resealed update", &resealed, 8, "cpu0 refused")?;
        let mut below: Vec<_> = (0..8).map(t14).collect();
        below[3] = (3, "Intel signature 0x806c1 platform 7 found 0xbd current 0xbd running 0xbd");
        refused("a T14 CPU below Intel's update", &below, 8, "runs 0xbe")?;
        let mut guest: Vec<_> = (0..8).map(t14).collect();
        guest[1] = (1, "Intel signature 0x806c1 found 0xbe loads nothing: a hypervisor owns this CPU's microcode running 0xbe");
        refused("a T14 CPU that loads nothing", &guest, 8, "runs 0xbe")?;
        let none = "[ 1.5] Microcode: ROOT holds no /system/share/microcode/intel/06-8c-01/80: none handed over\n";
        let handed_none = (0..8)
            .map(|cpu| (cpu, "Intel signature 0x806c1 platform 7 found 0xbe loads nothing: the loader handed no patch running 0xbe"))
            .collect::<Vec<_>>();
        match microcode_on_metal(Bench::T14, none, &log(&handed_none), 8) {
            Ok(()) => return Err("a T14 whose loader handed no patch was accepted".into()),
            Err(e) if e.contains("loader.log does not say") => {}
            Err(e) => return Err(format!("a T14 whose loader handed no patch was refused for the wrong reason: {e}")),
        }
        judged(Bench::T14, AMD_LOADER, "a T14 whose loader handed the AMD bench's patch", &(0..8).map(t14).collect::<Vec<_>>(), 8, "loader.log does not say")?;
        let flipped: Vec<_> = (0..8)
            .map(|cpu| (cpu, "Amd signature 0x860f01 found 0x8600106 refused 0x860010d with #GP(0x0) running 0x8600106"))
            .collect();
        amd_refused("an AMD bench whose CPUs refused a flipped patch", &flipped, "cpu0 refused")?;
        let mut unread: Vec<_> = (0..8).map(amd).collect();
        unread[5] = (5, "Amd signature 0x860f01 loads nothing: ToyOS has no Amd microcode loader");
        amd_refused("an AMD CPU with no loader", &unread, "loaded 0x860010d")?;
        let mut unwritten: Vec<_> = (0..8).map(amd).collect();
        unwritten[3] = (3, "Amd signature 0x860f01 found 0x860010d current 0x860010d running 0x860010d");
        amd_refused("an AMD CPU that did not write its patch", &unwritten, "loaded 0x860010d")?;
        let mut other: Vec<_> = (0..8).map(t14).collect();
        other[2] = (2, "Intel signature 0x806c2 platform 7 found 0xbe current 0xbe running 0xbe");
        refused("a CPU of another signature than its bench's", &other, 8, "is not")?;
        amd_refused("the T14's lines read as the AMD bench's", &(0..8).map(t14).collect::<Vec<_>>(), "is not")?;
        eprintln!("  [microcode] the verdict refuses 12 machines and accepts both benches");
        Ok(())
    }

    /// [`mask_windows`] and `irqcensus::windows_under` against captures no
    /// boot has to produce: two CPUs and the T14 row's three exits, a report
    /// each, with cpu1 holding at the first.
    #[test]
    fn mask_windows_verdict() -> Result<(), String> {
        use common::irqcensus::{windows_under, Measured};
        let windows = |cpu: u32, (irqs, preempt): (u64, u64)| {
            format!("[ 0.100 cpu0 kernel] windows: cpu{cpu} irqs_off_ns={irqs} preempt_off_ns={preempt}\n")
        };
        let report = |cpu0: (u64, u64), cpu1: (u64, u64)| [windows(0, cpu0), windows(1, cpu1)].concat();
        let exit = |name: &str| format!("[ 0.100 cpu0 kernel] exit: {name} pid=9 code=0 cpu=1ms\n");
        let held_ns = kernel::sched::windows::HELD_NS;
        let hold = format!("[ 0.100 cpu1 kernel] windows: held cpu1 ns={}\n", held_ns + 7);
        // Since each CPU joined: longer on cpu0 than anything under the load.
        let first = [report((6_500_000, 6_400_000), (900, 800)), exit("test_rs_idle_span")].concat();
        let read_back = (held_ns + 400, held_ns + 300);
        let opening = [report((100, 100), read_back), exit("pwd")].concat();
        let own = [report((3_000_000, 1_100_000), (700_000, 600_000)), exit(WINDOWS_LOAD)].concat();
        let good = [hold.as_str(), &first, &opening, &own].concat();
        let exited = exited(WINDOWS_LOAD);

        mask_windows(&good, 2).map_err(|e| format!("the good capture was refused: {e}"))?;
        let unpaired = |what: &str, capture: &str, says: &str| match mask_windows(capture, 2) {
            Ok(()) => Err(format!("{what} was accepted")),
            Err(e) if e.contains(says) => Ok(()),
            Err(e) => Err(format!("{what} was refused for the wrong reason: {e}")),
        };
        unpaired("a report without one of its CPUs", &good.replace(&windows(0, (100, 100)), ""), "went out without")?;
        unpaired("a CPU that closed no window", &report((0, 7), (3, 4)), "closed no window")?;
        unpaired("one CPU of two", &windows(0, (5, 7)), "1 of 2")?;
        unpaired("a field missing", &good.replace(" preempt_off_ns=100", ""), "fields")?;

        let read = windows_under(&good, 2, &exited)?;
        let want = Measured { held: read_back, load: (3_000_000, 1_100_000) };
        if read != want {
            return Err(format!("the good capture read {read:?}, and the hold's and the load's reports say {want:?}"));
        }
        let refused = |what: &str, capture: &str, says: &str| match windows_under(capture, 2, &exited) {
            Ok(read) => Err(format!("{what} was read as {read:?}")),
            Err(e) if e.contains(says) => Ok(()),
            Err(e) => Err(format!("{what} was refused for the wrong reason: {e}")),
        };
        refused("a load's report with none before it", &[hold.as_str(), &own].concat(), "the boot's first")?;
        refused("a load that never ended", &good.replace(&exit(WINDOWS_LOAD), ""), "never ended")?;
        refused("no hold", &good.replace(&hold, ""), "held no window of known length")?;
        refused("a second hold", &[hold.as_str(), &good].concat(), "a second hold")?;
        refused("a hold cut short", &good.replace(&hold, "[ 0.100 cpu1 kernel] windows: held cpu1 ns=1000\n"), "the kernel owes")?;
        refused(
            "an interrupts-off window read back at half",
            &good.replace(&windows(1, read_back), &windows(1, (read_back.0 / 2, read_back.1))),
            "shorter than it was held",
        )?;
        refused(
            "a preemption-off window read back at half",
            &good.replace(&windows(1, read_back), &windows(1, (read_back.0, read_back.1 / 2))),
            "shorter than it was held",
        )?;
        refused(
            "an interrupts-off window read back at ten times",
            &good.replace(&windows(1, read_back), &windows(1, (read_back.0 * 10, read_back.1))),
            "past the ceiling",
        )?;
        refused(
            "a preemption-off window read back at ten times",
            &good.replace(&windows(1, read_back), &windows(1, (read_back.0, read_back.1 * 10))),
            "past the ceiling",
        )?;
        refused(
            "a hold the load's own report reads back",
            &[first.as_str(), &opening, &hold, &own].concat(),
            "reported nothing between",
        )?;
        refused("a report that skips a CPU", &good.replace(&windows(0, (3_000_000, 1_100_000)), ""), "names cpu1 at place 0")?;
        Ok(())
    }

    /// A line dated by the loader's reading carried on the log's clock
    /// passes, to the second it lands in; an anchor ten seconds off is red,
    /// whichever way; and a bench whose clock stands two hours east carries
    /// its reading as well as any, and is red as the bench's.
    #[test]
    fn a_date_off_the_loaders_reading_is_red_and_a_bench_off_utc_is_the_benchs() {
        const RATE: &str = "[10.300 cpu0 kernel] clock: the TSC counts at 1000000000 Hz, as CPUID 15H states, held to the HPET from a reading 90 cycles wide\n";
        // Read 10.000 s on the log's clock, so a line at 12.165 is dated two seconds on.
        let loader = |read: &str| format!("[10.000 cpu0 loader] Wall clock: firmware's GetTime reads {read} UTC at counter 10000000000\n");
        let log = |dated: &str| format!("[{dated} 12.165 cpu0 kernel] Boot: complete (1165ms)\n");
        let carried = |read: &str, dated: &str| dates_carry_the_loaders_reading(&loader(read), RATE, &log(dated));
        let read = carried("2026-09-29 18:22:37", "2026-09-29 18:22:39").expect("a line dated by the reading");
        for dated in ["2026-09-29 18:22:37", "2026-09-29 18:22:40"] {
            assert_eq!(carried("2026-09-29 18:22:37", dated), Ok(read), "{dated} is inside what the divisions drop");
        }
        for dated in ["2026-09-29 18:22:49", "2026-09-29 18:22:29", "2026-09-29 18:22:36", "2026-09-29 18:22:41"] {
            let refused = carried("2026-09-29 18:22:37", dated).expect_err(dated);
            assert!(refused.contains("from the loader's reading"), "{refused}");
        }
        assert!(dates_carry_the_loaders_reading(&loader("2026-09-29 18:22:37"), RATE, "[12.165 cpu0 kernel] undated\n").is_err());

        let boot = metal::Readback::new("boot", String::new(), metal_checks::ON_T14.into(), &metal_checks::boot_file())
            .expect("a readback");
        assert_eq!(bench_clock_keeps_utc(read, boot.ran_within()), Ok(()));
        let east = carried("2026-09-29 20:22:37", "2026-09-29 20:22:39").expect("a bench off UTC carries its reading too");
        for off in [east, read - 900] {
            let refused = bench_clock_keeps_utc(off, boot.ran_within()).expect_err("a bench off UTC");
            assert!(refused.contains("this bench's hardware clock stands"), "{refused}");
        }
    }

    /// The staged stall's row reads the staged span alone: left out with the
    /// PM timer 10 ms from the HPET and a report after it, on a bench whose
    /// TSC steps for real in another span or in that one; and red where the
    /// span was read as the TSC's, where the PM timer stood with the HPET, or
    /// never, or no report followed.
    #[test]
    fn a_staged_stall_is_judged_by_its_own_span() {
        const FIRST: &str = "[11.370 cpu0 kernel] clock: over 1 s the TSC counted 2419185510351 mHz against the HPET, within 11437 ppb; the clock keeps CPUID 15H's 2419200000000 mHz; 0 spans left out; 0 TSC step(s) slewed\n";
        const STAGED: &str = "[11.370 cpu0 kernel] hpet-stall: the HPET reads 192000 ticks behind from here\n";
        const MOVED: &str = "[12.375 cpu3 kernel] clock: the HPET moved and the TSC did not, so this span is left out: the TSC counted 2431296000 ticks, the HPET 995001000 ns and reads 9999052 ns apart, 523256 allowed (a to b); the PM timer counted 1005000100 ns and reads 1487 ns apart, 516738 allowed (c to d)\n";
        const REPORT: &str = "[13.380 cpu1 kernel] clock: over 2 s the TSC counted 2419185510351 mHz against the HPET, within 5718 ppb; the clock keeps CPUID 15H's 2419200000000 mHz; 1 spans left out; 0 TSC step(s) slewed\n";
        const REAL_STEP: &str = "[ 9.120 cpu5 kernel] clock: TSC STEP: the TSC stepped 1718866 ns forward against the HPET and the PM timer; the clock slews it away at most 500 ppm, most of it in about 3437 ms: the TSC counted 1799000000 ticks, the HPET 998281134 ns and reads 1718866 ns apart, 520000 allowed (a to b); the PM timer counted 998281000 ns and reads 1718700 ns apart, 518000 allowed (c to d)\n";
        const AS_THE_TSCS: &str = "[12.375 cpu3 kernel] clock: TSC STEP: the TSC stepped 9999052 ns forward against the HPET; the clock slews it away at most 500 ppm, most of it in about 19998 ms: the TSC counted 2431296000 ticks, the HPET 995001000 ns and reads 9999052 ns apart, 523256 allowed (a to b); no PM timer\n";
        const ALL: &str = "[12.375 cpu3 kernel] clock: the TSC, the HPET and the ACPI PM timer all disagree, so this span is left out: the TSC counted 2431296000 ticks, the HPET 995001000 ns and reads 11717918 ns apart, 523256 allowed (a to b); the PM timer counted 1003281134 ns and reads 1718866 ns apart, 516738 allowed (c to d)\n";
        let judged = |lines: &[&str]| faults::stalled_hpet_left_out(&lines.concat());

        assert_eq!(judged(&[FIRST, STAGED, MOVED, REPORT]), Ok(()));
        // The AMD bench: its own step before the staging, or after the staged span was read.
        assert_eq!(judged(&[REAL_STEP, FIRST, STAGED, MOVED, REPORT]), Ok(()));
        assert_eq!(judged(&[FIRST, STAGED, MOVED, REAL_STEP, REPORT]), Ok(()));
        // Its own step inside the staged span: every counter disagrees, and the PM timer still tells the stall.
        assert_eq!(judged(&[FIRST, STAGED, ALL, REPORT]), Ok(()));
        let untold = ALL.replace("reads 1718866 ns apart", "reads 9100000 ns apart");
        for (why, lines) in [
            ("read as the TSC's", &[FIRST, STAGED, AS_THE_TSCS, REPORT][..]),
            ("a PM timer that stands with neither", &[FIRST, STAGED, untold.as_str(), REPORT]),
            ("the bench's step read where the stall should be", &[FIRST, STAGED, REAL_STEP, MOVED, REPORT]),
            ("no report after", &[FIRST, STAGED, MOVED]),
            ("never read", &[FIRST, STAGED]),
            ("never staged", &[FIRST, MOVED, REPORT]),
        ] {
            assert!(judged(lines).is_err(), "{why} judged green");
        }
    }

    /// A slewed step passes the clock's rows and a demotion fails them.
    #[test]
    fn a_demoted_clock_fails_its_rows() {
        let slewed = "[ 20.778 cpu12 kernel] clock: TSC STEP: the TSC stepped 1695132 ns forward against the HPET\n";
        assert_eq!(clock_kept_its_counter(slewed), Ok(()));
        let demoted = format!("{slewed}[ 33.252 cpu1 kernel] clock: DEMOTED: the TSC stepped 10000110 ns\n");
        assert!(clock_kept_its_counter(&demoted).is_err());
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
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/guest/rust-tests/src/bin");
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

        eprintln!(
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

        // Other work beside the run outranks a red, and a quiet host with other
        // work under a CPU on average changes nothing.
        let busy = common::hostload::Reading { other: Duration::from_secs(61), over: Duration::from_secs(60) };
        let calm = common::hostload::Reading { other: Duration::from_secs(59), over: Duration::from_secs(60) };
        let mut noisy = Tally::new();
        noisy.record(outcome("a_red", Some("the disk came back short"), Duration::ZERO));
        noisy.host = Some(busy);
        let text = noisy.summary(1, Duration::from_secs(60), Duration::ZERO);
        if noisy.exit_code() != 3 || !text.contains("test result: HOST NOT QUIET.") {
            return Err(format!("a run beside other work exits {}:\n{text}", noisy.exit_code()));
        }
        noisy.host = Some(calm);
        if noisy.exit_code() != 1 {
            return Err(format!("a red on a quiet host exits {}, and it has to be 1", noisy.exit_code()));
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

    /// A run takes every declared test its filter matches, of either
    /// architecture.
    #[test]
    fn a_run_selects_by_filter() -> Result<(), String> {
        let taken = |filters: &[&str]| -> BTreeSet<String> {
            let (machine, screen) = select(filters);
            machine
                .iter()
                .map(|(n, _)| n.to_string())
                .chain(screen.iter().map(|(n, _, _)| n.to_string()))
                .collect()
        };
        let names = |of: &[&str]| -> BTreeSet<String> { of.iter().map(|n| n.to_string()).collect() };
        let every: BTreeSet<String> = declared().filter(|n| *n != "hosted_clang_hello").map(String::from).collect();
        let cases: [(&[&str], _); 7] = [
            (&[], every),
            (&["hosted_clang_hello"], names(&["hosted_clang_hello"])),
            (&["virt_el2"], names(&["virt_el2_drop"])),
            (&["el2_drop"], names(&["virt_el2_drop"])),
            (&["nested_nmi"], names(&["nested_nmi_is_loud"])),
            (&["el2_drop", "nested_nmi"], names(&["virt_el2_drop", "nested_nmi_is_loud"])),
            (&["no_such_test"], BTreeSet::new()),
        ];
        for (filter, want) in cases {
            let got = taken(filter);
            if got != want {
                return Err(format!(
                    "filter {filter:?}: took {:?} it should not and left out {:?}",
                    got.difference(&want).collect::<Vec<_>>(),
                    want.difference(&got).collect::<Vec<_>>()
                ));
            }
        }
        Ok(())
    }

    /// Two rows under one name are refused, whether both are shared-boot names
    /// or one is a declared registry's or the metal table's.
    #[test]
    fn a_name_registered_twice_is_refused() -> Result<(), String> {
        let shared = |of: &[&str]| -> Vec<String> { of.iter().map(|n| n.to_string()).collect() };
        let apart = shared(&["shared_one", "shared_two"]);
        let names = registered(&apart)?;
        for name in ["shared_one", "shared_two", "virt_el2_drop", "control_regs"] {
            if !names.contains(name) {
                return Err(format!("{name} is not among the {} registered names", names.len()));
            }
        }
        for twice in [
            shared(&["shared_one", "shared_one"]),
            shared(&["shared_one", "virt_el2_drop"]),
            shared(&["shared_one", "control_regs"]),
        ] {
            let twice_name = &twice[1];
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
    fn metal_audio_judges() -> Result<(), String> {
        audio_checks::judges_verdict()
    }

    #[test]
    fn metal_claim_spends_one_remapping_entry() {
        claims_checks::one_entry_per_slot();
    }

    #[test]
    fn metal_domains_map_clear_of_the_host_bridges() {
        claims_checks::domains_map_clear_of_the_windows();
    }

    #[test]
    fn metal_name_measured_twice_is_refused() {
        metal_checks::a_name_measured_twice_is_refused();
    }

    #[test]
    fn metal_number_fails_with_its_owner_alone() {
        metal_checks::a_number_fails_with_its_owner_alone();
    }

    #[test]
    fn metal_boot_that_lost_parts_of_its_log_judges_no_row() {
        metal_checks::a_boot_that_lost_parts_of_its_log_judges_no_row();
    }

    #[test]
    fn metal_name_two_owners_measured_is_refused() {
        metal_checks::a_name_two_owners_measured_is_refused();
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
    fn metal_readbacks_of_a_machine_with_no_record_are_red() {
        metal_checks::readbacks_of_a_machine_with_no_record_are_red_and_record_nothing();
    }

    #[test]
    fn metal_bench_disk_judge_admits_data_and_the_loaders_volumes_alone() {
        metal_checks::the_bench_disk_judge_admits_data_and_the_loaders_volumes_alone();
    }

    #[test]
    fn metal_bench_reaches_the_loop_as_its_host() {
        metal_checks::the_bench_reaches_the_loop_as_its_host();
    }

    #[test]
    fn metal_exit_record_without_a_code_answers_no_code() {
        metal_checks::an_exit_record_without_a_code_answers_no_code();
    }

    #[test]
    fn metal_failing_shared_member_fails_itself_alone() {
        metal_checks::a_failing_shared_member_fails_itself_alone();
    }

    #[test]
    fn metal_boots_last_job_is_behind_every_other() {
        metal_checks::a_boots_last_job_is_behind_every_other();
    }

    #[test]
    fn metal_rows_run_before_members_under_a_bound_the_members_widen() {
        metal_checks::rows_run_before_members_under_a_bound_the_members_widen();
    }

    #[test]
    fn metal_words_take_rows_members_and_whole_boots() {
        metal_checks::words_take_rows_members_and_whole_boots();
    }

    #[test]
    fn metal_bench_takes_its_own_rows_and_names_the_rest() {
        metal_checks::a_bench_takes_its_own_rows_and_names_the_rest();
    }

    #[test]
    fn metal_profile_gives_each_bench_its_own_jobs() {
        metal_checks::the_profile_gives_each_bench_its_own_jobs();
    }

    #[test]
    fn metal_runs_bench_is_its_readbacks_and_else_the_lines() {
        metal_checks::a_runs_bench_is_its_readbacks_and_else_the_lines();
    }

    #[test]
    fn metal_judge_names_the_bench_it_judged() {
        metal_checks::a_judge_names_the_bench_it_judged();
    }

    #[test]
    fn metal_each_benchs_own_readback_is_judged_as_that_bench() {
        metal_checks::each_benchs_own_readback_is_judged_as_that_bench();
    }

    #[test]
    fn metal_loader_kernel_and_program_count_from_one_zero() {
        metal_checks::the_loader_the_kernel_and_a_program_count_from_one_zero();
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
            .find(|(name, _)| *name == "memory_overwrite_requested")
            .map(|(name, decl)| vec![(*name, decl)])
            .ok_or_else(|| "memory_overwrite_requested is not registered".to_string())?;
        let taken = metal::Taken { rows: selected, shared: Vec::new(), left: Vec::new() };
        let verdict = metal::run(mode, Bench::T14, Bench::T14.host(), &taken, &[], true);
        if verdict != metal::Verdict::Green {
            return Err(format!("--metal --list produced {verdict:?}, not Green"));
        }
        Ok(())
    }

    /// The judge a metal registration runs.
    fn metal_judge(name: &str) -> fn(&[&metal::Readback]) -> Result<(), String> {
        match METAL.iter().find(|(row, _)| *row == name) {
            Some((_, metal::Metal { judge, .. })) => *judge,
            None => panic!("{name} runs no metal judge"),
        }
    }

    /// One T14 boot's readback, out of a `loader.log` and a `logkeeper` text.
    fn readback(label: &str, loader: &str, log: &str) -> metal::Readback {
        let log = format!("{}{log}", metal_checks::ON_T14);
        metal::Readback::new(label, loader.into(), log, &metal_checks::boot_file())
            .expect("a boot file as the loop writes it")
    }

    /// The pass before the handoff, which every `loader.log` opens with.
    const HANDOFF: &str = "Loader log: the kernel handoff begins, so this file ends here\n";

    /// The judges that read the loader's file and the kernel's records off the
    /// volume, fed a T14 readback's lines: the memory-overwrite request the
    /// loader set, the reset register's decode, and the disk load's lines under
    /// a boot its deadline ended rather than one that stopped.
    #[test]
    fn metal_judges_read_the_volume() {
        let judge = metal_judge("memory_overwrite_requested");
        let set = format!(
            "{HANDOFF}[ 9.809 cpu0 loader] Memory overwrite: MemoryOverwriteRequestControl read [00] \
             (attributes 0x7) and is now [01], so firmware overwrites memory before the next boot\n"
        );
        assert_eq!(judge(&[&readback("testcases", &set, "")]), Ok(()));
        let refused = set.replace("read [00]", "reads [00]").replace("and is now [01]", "and firmware refused [01] (WRITE_PROTECTED)");
        assert!(judge(&[&readback("testcases", &refused, "")]).is_err());
        let undefined = format!(
            "{HANDOFF}Memory overwrite: firmware defines no MemoryOverwriteRequestControl, so it \
             promises no wipe after a reset and nothing is written\n"
        );
        assert!(judge(&[&readback("testcases", &undefined, "")]).is_err());

        let jobcase =
            "[2026-09-29 10:40:36  0.000 cpu0 kernel] ACPI: reset register SystemIO 0xcf9 <- 0x06\n";
        let judge = metal_judge("machine_reboot");
        assert_eq!(judge(&[&readback("jobcase", HANDOFF, jobcase)]), Ok(()));
        let none = "[2026-09-29 10:40:36  0.000 cpu0 kernel] ACPI: no reset register this kernel writes\n";
        assert!(judge(&[&readback("jobcase", HANDOFF, none)]).is_err());

        let judge = metal_judge("the_disk_survives_a_reset_under_load");
        let kernel = "[2026-09-29 10:33:28  0.000 cpu0 kernel] panic console: armed 1920x1080 \
                      stride=1920 format=1 at 0x4000000000, write-combining\n";
        let line = |n: u32| format!("[2026-09-29 10:33:40 20.100 test-runner pid=12] stick_load: line {n} {}\n", "x".repeat(840));
        let wrote = format!("{kernel}{}", (0..1200).map(line).collect::<String>());
        let short = format!("{kernel}{}", (0..10).map(line).collect::<String>());
        assert!(judge(&[&readback("usbload", HANDOFF, &short)]).is_err(), "ten lines are no mebibyte");
        assert_eq!(judge(&[&readback("usbload", HANDOFF, &wrote)]), Ok(()));
        let stopped = format!("{wrote}[2026-09-29 10:33:52 32.000 supervisor] {} (Reboot)\n", bootlog::STOPPING);
        assert!(judge(&[&readback("usbload", HANDOFF, &stopped)]).is_err(), "a boot that stopped had no reset under load");
    }

    /// A member that reads another test binary by path.
    #[test]
    fn a_shared_chunk_stages_every_binary_its_members_name() {
        let source = "const CHILD: &str = \"/system/bin/test_rs_child\";\n";
        let bins: Vec<(String, Vec<u8>)> = ["parent", "child", "stranger", "libfoo.so"]
            .iter()
            .map(|name| ((*name).to_string(), Vec::new()))
            .collect();
        let jobs = vec!["test_rs_parent --write".to_string()];
        let staged = |text: &str| -> Vec<String> {
            metal::reached(text, &jobs, &bins).into_iter().map(|(path, _)| path).collect()
        };
        assert_eq!(
            staged(&format!("test_rs_parent\n{source}")),
            ["bin/test_rs_child", "lib/libfoo.so"]
        );
        // A longer name is another binary's.
        let longer = source.replace("test_rs_child", "test_rs_child_two");
        assert_eq!(staged(&format!("test_rs_parent\n{longer}")), ["lib/libfoo.so"]);
    }

    /// `03_struct`'s output as `ccheck` prints it: the expectation the corpus
    /// stages is that, under the guest's `trim_end`.
    #[test]
    fn the_c_corpus_stages_the_expectation_the_host_compares() {
        let got = "12\n34\n12\n34\n56\n78\n~fred()";
        let boot = c_corpus_metal(&[("03_struct".to_string(), Vec::new())]);
        let staged = boot
            .files
            .iter()
            .find(|(path, _)| path == "expect/03_struct")
            .map(|(_, bytes)| String::from_utf8(bytes.clone()).expect("an expectation is text"))
            .expect("03_struct's expectation is staged");
        assert_eq!(staged.trim_end(), got);
    }

    /// `arm_psci_call` as QEMU 11.1.1's `trace-events` formats it, with and
    /// without the log backend's `pid@time:` head, and every way a power-off's
    /// trace falls short of PSCI's recipe refused, a CPU left on calling
    /// `CPU_OFF` among them.
    #[test]
    fn psci_power_off_judge() {
        let line = |head: &str, function: u64, cpu: u64| {
            format!("{head}arm_psci_call PSCI Call x0=0x{function:016x} x1=0x0000000000000000 x2=0x0000000000000000 x3=0x0000000000000000 cpuid=0x{cpu:x}")
        };
        let text = [line("", PSCI_CPU_OFF, 1), line("4242@1759272000.123456:", PSCI_SYSTEM_OFF, 0)].join("\n");
        assert_eq!(psci_calls(&text), Ok(vec![(PSCI_CPU_OFF, 1), (PSCI_SYSTEM_OFF, 0)]));
        assert!(psci_calls("arm_psci_call PSCI Call x0=?").is_err());

        let (off, system_off) = (PSCI_CPU_OFF, PSCI_SYSTEM_OFF);
        let affinity_info = 0xC400_0004;
        assert_eq!(psci_powered_off(&[(off, 1), (affinity_info, 3), (off, 2), (off, 0), (system_off, 3)], 4, &[]), Ok(3));
        assert_eq!(psci_powered_off(&[(off, 1), (off, 0), (system_off, 3)], 4, &[2]), Ok(3));
        for short in [
            vec![(off, 1), (off, 0), (system_off, 3)],
            vec![(off, 1), (off, 0), (system_off, 3), (off, 2)],
            vec![(off, 1), (off, 1), (off, 2), (off, 0), (system_off, 3)],
            vec![(off, 1), (off, 2), (off, 0)],
            vec![(off, 1), (off, 2), (off, 0), (system_off, 3), (system_off, 3)],
            vec![(off, 1), (off, 2), (off, 0), (PSCI_SYSTEM_RESET, 3), (system_off, 3)],
            vec![(off, 1), (off, 2), (off, 3), (system_off, 3)],
        ] {
            assert!(psci_powered_off(&short, 4, &[]).is_err(), "{short:x?}");
        }
        assert!(psci_powered_off(&[(off, 1), (off, 2), (off, 0), (system_off, 3)], 4, &[2]).is_err());
    }
}
