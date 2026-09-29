use super::*;
use serial::*;

/// What the one answer is made of, for the gate that keeps it the only one.
///
/// `one_vocabulary` in `tests/checks.rs` refuses a wait that hands any of these
/// straight to a scan of its own; it reads them from here so that it cannot
/// become a second, staler copy of the list it is protecting.
pub fn spellings() -> impl Iterator<Item = &'static str> {
    DEATHS.iter().map(|(word, _, _)| *word)
}

/// Prove the vocabulary in both directions, with no guest.
///
/// `screen_decoder` does this for the framebuffer decoder — an instrument
/// nothing else checks is an instrument nobody knows is broken. Every case
/// here is one this type must *fail*, because the failures are the point: a
/// `must_not_say` that returns `Ok` on an empty capture is the whole hazard.
pub fn self_check() -> Result<(), String> {
    let live = Serial::named("test capture", "[kernel 0.001 cpu0] NVMe: found\nhello from userland\n");
    let dead = Serial::named("test capture", "");
    // Userland said things; the kernel said nothing. This is what a broken
    // capture looks like when it is not simply empty, and the case a
    // `text.is_empty()` guard would wave through.
    let mute = Serial::named("test capture", "hello from userland\n");
    let panicking = Serial::named(
        "test capture",
        "[kernel 0.001 cpu0] NVMe: found\n[kernel 0.002 cpu0] PANIC: nope\n",
    );

    /// One row: what it is called, whether it must pass, and the call itself.
    type Case<'a> = (&'a str, bool, &'a dyn Fn() -> Result<(), String>);

    let cases: &[Case] = &[
        // must_say
        ("must_say finds a line", true, &|| live.must_say("NVMe: found").map(|_| ())),
        ("must_say on an absent line", false, &|| live.must_say("no such line").map(|_| ())),
        ("must_say on a dead channel", false, &|| dead.must_say("anything").map(|_| ())),
        // must_not_say: the absent case passes only because the channel is alive
        ("must_not_say on an absent line", true, &|| live.must_not_say("no such line")),
        ("must_not_say on a present line", false, &|| live.must_not_say("NVMe: found")),
        // The dead gate itself, from both directions.
        ("must_not_say on an empty capture", false, &|| dead.must_not_say("anything")),
        ("must_not_say with no kernel output", false, &|| mute.must_not_say("anything")),
        // must_be_clean
        ("must_be_clean on a clean boot", true, &|| live.must_be_clean()),
        ("must_be_clean on a panic", false, &|| panicking.must_be_clean()),
        // The arm that closes what narrowing the death needle to `owner=kernel`
        // opened: a fault on a stream a process drives is not a death, and it
        // is still not a clean boot.
        ("must_be_clean on a user-owned DMA fault", false, &|| {
            Serial::named(
                "test capture",
                "[kernel 0.001 cpu0] NVMe: found\n[kernel 4.100 cpu0] iommu: DMA FAULT \
                 owner=slot0 unit0 stream=00:03.0 addr=0x1000 access=read reason=0x06 \
                 read-permission\n",
            )
            .must_be_clean()
        }),
        ("must_be_clean on an empty capture", false, &|| dead.must_be_clean()),
    ];

    for (what, want_ok, run) in cases {
        let got = run();
        if got.is_ok() != *want_ok {
            return Err(format!(
                "{what}: wanted {}, got {got:?}",
                if *want_ok { "Ok" } else { "Err" }
            ));
        }
    }

    // must_say hands back the line, not just a yes.
    let line = live.must_say("NVMe")?;
    if !line.contains("cpu0") {
        return Err(format!("must_say returned {line:?}, not the whole line"));
    }

    // `must_say_after`, against the capture that made it exist: a stranger line
    // of the right shape before the marker, and the test's own after it. The
    // first case is the defect and is asserted in both directions — the plain
    // scan reads the stranger, which is what `i8042_undecoded_bytes` did.
    const READY: &str = "===I8042_READY===";
    let stranger = "[kernel 0.418 cpu1] i8042: 1 interrupts and 0 bytes, nothing decoded — first \
                    seen at 418ms";
    let mine = "[kernel 2.816 cpu0] i8042: 2 interrupts and 6 bytes, nothing decoded — no event \
                from [0xe1, 0x1d, 0x45, 0xe1, 0x9d, 0xc5], first seen at 2816ms";
    let staged = Serial::named("test capture", format!("{stranger}\n{READY}\n{mine}\n"));
    if staged.must_say("nothing decoded")? != stranger {
        return Err(String::from("the whole-capture scan stopped reading the earliest line"));
    }
    if staged.must_say_after(READY, "nothing decoded")? != mine {
        return Err(format!("must_say_after read a line from before {READY:?}"));
    }
    // And with only the stranger in the capture there is no answer at all,
    // rather than the stranger: a test that staged nothing must not pass on
    // somebody else's line.
    let only_stranger = Serial::named("test capture", format!("{stranger}\n{READY}\n"));
    let err = only_stranger.must_say_after(READY, "nothing decoded").unwrap_err();
    if !err.contains("before") {
        return Err(format!("a stranger-only capture failed without naming why: {err}"));
    }
    // A missing anchor is a failure, not a fallback to the whole capture.
    let no_marker = Serial::named("test capture", format!("{stranger}\n"));
    if no_marker.must_say_after(READY, "nothing decoded").is_ok() {
        return Err(String::from("must_say_after answered from a capture with no marker in it"));
    }

    // Interleaving is detected and named, and a clean capture reports none.
    let split = Serial::named("test capture", "[kernel 0.001 cpu0] a\nBoot: comp[kernel 0.002 cpu0] lete\n");
    if split.interleaved().is_none() {
        return Err(String::from("a kernel prefix spliced mid-line was not detected"));
    }
    if live.interleaved().is_some() {
        return Err(String::from("a clean capture was reported as interleaved"));
    }
    // And a needle the interleaving split says so rather than "never said it".
    let err = split.must_say("Boot: complete").unwrap_err();
    if !err.contains("interleaved") {
        return Err(format!("a split needle failed without naming the cause: {err}"));
    }

    // **Who died, and the case the naive fix gets wrong.** A wait that ends a
    // run on a bare panic spelling ends it on a *program's* panic too, and a
    // program is expected to be able to die without killing the machine. The
    // prefix is the whole discriminator, so it is asserted from both sides:
    // the same words, once from the kernel and once from somebody else.
    const KERNEL_PANIC_LINE: &str =
        "[kernel 1.450 cpu3] PANIC: panicked at kernel/src/sched/reserve.rs:812:9:";
    const USER_PANIC_LINE: &str = "thread 'main' (1) panicked at sshd/src/main.rs:359:23:";
    let whose: &[(&str, Option<Died>)] = &[
        // The kernel, about itself.
        (KERNEL_PANIC_LINE, Some(Died::Kernel)),
        ("[kernel 0.068 cpu0] KERNEL PANIC: read unmapped address at 0x0", Some(Died::Kernel)),
        // The three that write none of the words beside them. The first is
        // verbatim what a staged `#DF` put on the console.
        (
            "[kernel 0.443 cpu0] DOUBLE FAULT on CPU 0 (pid=Some(Pid(5)) tid=Some(Tid(0)))",
            Some(Died::Kernel),
        ),
        ("[kernel 0.443 cpu0] MACHINE CHECK on CPU 3", Some(Died::Kernel)),
        (
            "[kernel 4.100 cpu0] iommu: DMA FAULT owner=kernel unit0 stream=00:1f.2 \
             addr=0x1000 access=read reason=0x06 unknown",
            Some(Died::Kernel),
        ),
        // The same fault on a stream a *process* drives. The machine is still
        // running, so the vocabulary must not read this as a death — and the
        // two lines differ in one field, which is what makes the case worth
        // stating rather than assuming.
        (
            "[kernel 4.100 cpu0] iommu: DMA FAULT owner=slot0 unit0 stream=00:03.0 \
             addr=0x1000 access=read reason=0x06 read-permission",
            None,
        ),
        ("[kernel 0.001 cpu0] EARLY PANIC: nothing is up yet", Some(Died::Kernel)),
        (
            "[kernel 2.000 cpu1] DOUBLE PANIC: the cpu was already in Fatal; first: invalid \
             opcode rip=0x0000000000401234 cr2=0x0000000000000000 err=0x0000000000000000; \
             second: panic at src/mm/paging.rs:41:5: the page is not there",
            Some(Died::Kernel),
        ),
        // No prefix, and still the kernel's: the reentry line goes out the UART
        // port directly, and no program in this tree says these words.
        ("\n!!! PANIC REENTRY: CPU halted !!! (apic 3)", Some(Died::Kernel)),
        ("KERNEL PANIC: spliced onto somebody's unterminated write", Some(Died::Kernel)),
        // The kernel, about a process. Its line, somebody else's death.
        ("[kernel 0.412 cpu0] SEGFAULT tid=7: read unmapped address at 0x0", Some(Died::Faulted)),
        ("[kernel 0.412 cpu0] SIGILL tid=7: illegal instruction", Some(Died::Faulted)),
        ("[kernel 0.412 cpu0] FATAL tid=7: machine check", Some(Died::Faulted)),
        // A process, about itself. Neither of these ends anybody's run.
        (USER_PANIC_LINE, Some(Died::Panicked)),
        ("libc panic: panicked at src/main.rs:9:1:", Some(Died::Panicked)),
        // The one the naive fix cannot tell from the kernel's, and must.
        ("PANIC: printed by a program that felt like printing it", Some(Died::Panicked)),
        // Nothing died.
        ("[kernel 0.377 cpu0] NVMe: found", None),
        ("hello from userland", None),
        ("", None),
    ];
    for (line, want) in whose {
        let got = died(line);
        if got != *want {
            return Err(format!("{line:?} reads as {got:?}, and it is {want:?}"));
        }
    }
    // The two spellings that decide it, side by side, from both speakers. Stated
    // as its own case because the table above would still pass if `died`
    // ignored the prefix and every kernel-written line simply came first.
    for word in ["PANIC:", "panicked at"] {
        let kernel = format!("[kernel 1.450 cpu3] {word} whatever follows");
        let program = format!("some program says {word} whatever follows");
        if died(&kernel) != Some(Died::Kernel) || died(&program) != Some(Died::Panicked) {
            return Err(format!(
                "{word:?} does not depend on who said it: kernel {:?}, program {:?}",
                died(&kernel),
                died(&program)
            ));
        }
    }
    // And `must_be_clean` still refuses both of them, because a boot that
    // carries either is not a clean boot whoever wrote it.
    for line in [KERNEL_PANIC_LINE, USER_PANIC_LINE] {
        let capture = Serial::named("test capture", format!("[kernel 0.001 cpu0] up\n{line}\n"));
        if capture.must_be_clean().is_ok() {
            return Err(format!("must_be_clean passed a capture carrying {line:?}"));
        }
    }

    // **The report, which is the artefact a verdict used to drop.** Staged as
    // the lines `double_fault_handler` really writes
    // (`kernel/src/arch/x86_64/idt/exceptions.rs`), with the ordinary run in front of
    // it and a daemon still talking after the header — a capture that begins at
    // the death would be a capture nobody has.
    const DF_HEADER: &str =
        "[kernel 6.204 cpu1] DOUBLE FAULT on CPU 1 (pid=Some(Pid(2)) tid=Some(Tid(0)))";
    let staged_df = format!(
        "[kernel 6.201 cpu0] spawn: /system/bin/test_rs_console_line_atomicity pid=41\n\
         AAAAAAAA\n\
         {DF_HEADER}\n\
         [kernel 6.204 cpu1]   cr2=0xffff800002672ff8 (address that caused the fault chain)\n\
         [kernel 6.204 cpu1]   rip=0xffffffff80121a40  rsp=0xffff800002673000  rbp=0x0\n\
         [kernel 6.204 cpu1]   Kernel backtrace:\n\
         soundd: suspended\n\
         [kernel 6.205 cpu1]   Found interrupt frame at stack offset +0x18:\n"
    );
    let Some(report) = death_report(&staged_df) else {
        return Err(String::from(
            "a capture carrying a whole double-fault report has no report in it, which is the \
             verdict that threw one away",
        ));
    };
    if !report.starts_with(DF_HEADER) {
        return Err(format!("the report does not start at the death line:\n{report}"));
    }
    // The body, and not merely the header: quoting the sentence again is what
    // the old arms already did.
    for want in ["cr2=0xffff800002672ff8", "rip=0xffffffff80121a40", "Found interrupt frame"] {
        if !report.contains(want) {
            return Err(format!("the report drops {want:?}:\n{report}"));
        }
    }
    // Nothing from before the death, because that is the run going normally and
    // a bound spent on it is a bound not spent on the report.
    if report.contains("spawn: /system/bin/test_rs_console_line_atomicity") {
        return Err(format!("the report starts before the death:\n{report}"));
    }
    // A line another process wrote *after* the header stays: the console is not
    // line-atomic and a report with holes cut in it is worse than one with a
    // daemon's line in the middle.
    if !report.contains("soundd: suspended") {
        return Err(format!("the report drops the lines it did not recognise:\n{report}"));
    }
    // The other direction, and the one that keeps this out of everybody's
    // terminal: a capture nothing died in has no report at all.
    let healthy = "[kernel 0.377 cpu0] NVMe: found\nBoot: complete\n";
    if death_report(healthy).is_some() {
        return Err(String::from("a clean capture produced a death report"));
    }
    // A *program* dying is not the machine's account either — the same
    // discrimination `died` makes, asked of the thing that quotes a capture.
    let program = format!("[kernel 0.001 cpu0] up\n{USER_PANIC_LINE}\nmore output\n");
    if death_report(&program).is_some() {
        return Err(format!("a program's own panic reads as the kernel's death:\n{program}"));
    }
    // Bounded, and it says by how much rather than trailing off. 400 lines of
    // report is four times what the deepest one in this tree writes.
    let flood: String = std::iter::once(DF_HEADER.to_string())
        .chain((0..400).map(|i| format!("[kernel 6.204 cpu1]   line {i}")))
        .collect::<Vec<_>>()
        .join("\n");
    let bounded = death_report(&flood).ok_or("a 401-line report vanished")?;
    if bounded.lines().count() > REPORT_LINES + 1 {
        return Err(format!(
            "the report is unbounded: {} lines of a {}-line capture",
            bounded.lines().count(),
            flood.lines().count()
        ));
    }
    if !bounded.contains("of the 401 lines that followed") {
        return Err(format!(
            "a truncated report does not say how much it dropped:\n{}",
            bounded.lines().next().unwrap_or_default()
        ));
    }

    eprintln!(
        "  [serial] {} vocabulary cases, both directions, plus the anchored scan against a \
         stranger line, {} lines classified by who said them, and a {}-line double-fault report \
         recovered from a capture that also carried a daemon and a program's panic",
        cases.len(),
        whose.len(),
        report.lines().count(),
    );
    Ok(())
}
