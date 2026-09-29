use super::*;
use qemu::*;

/// The oversubscription derivation, staged against known `(vcpus, cores)` pairs
/// with no guest at all — the oracle for [`budget_smp`]'s widening.
///
/// A measured bound is asserted against the derivation, never the other way
/// round (`tests/CLAUDE.md`): the numbers here are `vcpus/cores` and each case
/// says which host it is. It also pins the two ends that matter — the runner
/// widens and the dev host does not — and that the factor is finite, so a real
/// hang is still caught in bounded time.
pub fn host_scale_self_check() -> Result<(), String> {
    // The runner: eight vCPUs on four cores waits 8/4 = 2x longer before the
    // ceiling calls a still-progressing guest wedged.
    if oversub_ratio(8, 4) != (8, 4) {
        return Err(format!(
            "an eight-vCPU guest on the four-core runner must widen by 8/4, got {:?}",
            oversub_ratio(8, 4)
        ));
    }
    // The dev host: fourteen cores, so nothing in the suite (smp<=8) is
    // oversubscribed and the factor is 1 — this widens nothing locally.
    for (smp, cores) in [(2u32, 4u32), (8, 14), (2, 14), (8, 8)] {
        if oversub_ratio(smp, cores) != (1, 1) {
            return Err(format!(
                "smp={smp} on {cores} cores is not oversubscription (smp<=cores), yet the factor \
                 is {:?} rather than 1",
                oversub_ratio(smp, cores)
            ));
        }
    }
    // Finite in the worst case the suite can reach: eight vCPUs on a single
    // core is 8x, not unbounded — so a genuine hang still reports in bounded
    // time. `budget_smp` composes this with `budget`'s own capped host_scale
    // (<=8x) and phase width, and on the `--jobs 1` runner width is 1.
    if oversub_ratio(8, 1) != (8, 1) {
        return Err(format!("the worst suite case must stay finite at 8x, got {:?}", oversub_ratio(8, 1)));
    }
    eprintln!(
        "  [host-scale] oversubscription is vcpus/cores: 8-on-4 widens 2x, 8-on-14 not at all; \
         this host reports {} core(s)",
        host_cores()
    );
    Ok(())
}

/// The three verdicts a ceiling reaches and what each carries, staged with no
/// guest at all.
///
/// The gate for [`ceiling_verdict`], and it runs in both directions on each:
/// the panic must be named *and* not read as a stall, the stall must still read
/// as one, and a program's own panic must not end anybody's run. That last one
/// is the case the obvious patch breaks — a bare panic spelling in the read
/// loop matches a guest binary's panic, and a guest binary is allowed to die.
///
/// The fourth section is [`WaitVerdict`]: naming a death is not the same as
/// keeping the report, and a suite that had the first without the second lost a
/// double fault's whole account on 2026-08-18.
pub fn ceiling_self_check() -> Result<(), String> {
    const CEILING: Duration = Duration::from_secs(380);
    const KERNEL: &str =
        "[kernel 1.450 cpu3] PANIC: panicked at kernel/src/sched/reserve.rs:812:9:";
    let quiet = GUEST_QUIET + Duration::from_secs(1);
    let talking = Duration::from_millis(200);

    // 1. The kernel panicked and the machine went quiet. Named, and named
    //    *before* the ceiling: the guest died at 1.45 s and the guard is 380 s.
    let early = Duration::from_secs(17);
    let Some(panic) = ceiling_verdict(Some(KERNEL), early, CEILING, quiet, 40) else {
        return Err(String::from(
            "a kernel panic followed by silence did not end the wait, so it costs the whole guard",
        ));
    };
    if !panic.contains("kernel panic") || !panic.contains("reserve.rs:812:9") {
        return Err(format!("the verdict does not name the panic: {panic}"));
    }
    if panic.contains(STALLED) {
        return Err(format!("a kernel panic is still reported as a stall: {panic}"));
    }
    if early >= CEILING {
        return Err(String::from("staged the panic after the ceiling, so it proves nothing"));
    }
    let again = "[kernel 1.503 cpu7] PANIC: panicked at kernel/src/sched/reserve.rs:812:9:";
    if ceiling_verdict(Some(again), early, CEILING, quiet, 40).as_deref() != Some(panic.as_str()) {
        return Err(format!(
            "one panic on two boots gives two sentences:\n\
             {panic}\n{:?}",
            ceiling_verdict(Some(again), early, CEILING, quiet, 40)
        ));
    }

    // 2. A **userland** panic is not the machine's death. `died` is what the
    //    read loop asks, so the case is staged where the loop reads it: the
    //    same words from a program classify as nobody's business, and a wait
    //    with no kernel death in it runs on.
    const USER: &str = "thread 'main' (1) panicked at sshd/src/main.rs:359:23:";
    if super::serial::died(USER) == Some(super::serial::Died::Kernel) {
        return Err(format!("a program's own panic reads as the kernel's: {USER:?}"));
    }
    if ceiling_verdict(None, early, CEILING, quiet, 40).is_some() {
        return Err(String::from(
            "a run with no kernel death in it ended before its ceiling — a program that panicked \
             would take the whole test down with it",
        ));
    }

    // 2b. The same two cases for the wait that holds a whole capture rather
    //     than a line at a time — `await_guest`, whose `it went quiet` is the
    //     wording #156's signature is stated in.
    let halted = format!("[kernel 0.400 cpu0] compositor: frames=120\n{KERNEL}\n");
    let Some(found) = super::serial::kernel_death(&halted) else {
        return Err(String::from("a capture ending in a kernel panic reads as a guest that merely \
                                 stopped, which is the verdict that threw the cause away"));
    };
    if kernel_died_here(found) != panic {
        return Err(String::from("the two waits word one panic differently"));
    }
    let program_died = format!("[kernel 0.400 cpu0] compositor: frames=120\n{USER}\n");
    if super::serial::kernel_death(&program_died).is_some() {
        return Err(format!(
            "a capture whose only panic is a program's reads as a halted machine:\n{program_died}"
        ));
    }

    // 3. A guest that merely stopped, with no panic of either kind, still
    //    reports as a stall.
    let Some(stall) = ceiling_verdict(None, CEILING + Duration::from_secs(1), CEILING, quiet, 40)
    else {
        return Err(String::from("an expired guard on a silent guest returned no verdict at all"));
    };
    if !stall.starts_with(STALLED) {
        return Err(format!("a genuine stall stopped reporting as one: {stall}"));
    }
    // And the other end of the same guard: a guest still talking at the ceiling
    // was working, and that is a different red.
    let Some(slow) = ceiling_verdict(None, CEILING + Duration::from_secs(1), CEILING, talking, 900)
    else {
        return Err(String::from("an expired guard on a talking guest returned no verdict"));
    };
    if slow.contains(STALLED) || !slow.contains("did not finish") {
        return Err(format!("a slow test reads as a stall: {slow}"));
    }
    // Nothing has expired and nothing died: no verdict.
    if ceiling_verdict(None, early, CEILING, talking, 40).is_some() {
        return Err(String::from("a healthy run was given a verdict"));
    }

    // 3b. **The wall-clock/silence split this file's own defect was about**, in
    //     all four directions. A talking guest past its budget is slow, not
    //     wedged; a silent one within its budget is idle, not wedged; the wedge
    //     guard still fires, and fast; and the backstop still catches a guest
    //     that talks forever.
    const TIGHT: Duration = Duration::from_secs(153);
    let bstop = TIGHT.max(GUEST_WEDGED);
    assert!(TIGHT < bstop, "the case needs a ceiling below the backstop");
    // (a) The flake itself: `launcher_refusals` at `192s "still talking 1s ago"`
    //     on a loaded smp:2 runner. Past its 153 s budget, but talking — no
    //     verdict, it runs on.
    if ceiling_verdict(None, Duration::from_secs(192), TIGHT, Duration::from_secs(1), 500).is_some()
    {
        return Err(String::from(
            "a slow-but-talking guest past its budget was still called wedged — the smp:2 flake \
             this change is for",
        ));
    }
    // (b) The backstop still bites a guest that is stuck *and* chatty: past
    //     `GUEST_WEDGED`, still talking, it is the one thing silence cannot catch.
    let Some(forever) = ceiling_verdict(
        None,
        bstop + Duration::from_secs(1),
        TIGHT,
        Duration::from_secs(1),
        9000,
    ) else {
        return Err(String::from("a guest talking forever past the backstop was given no verdict"));
    };
    if forever.contains(STALLED) || !forever.contains("did not finish") {
        return Err(format!("the chatty-forever backstop misread as a stall: {forever}"));
    }
    // (c) Negative control — the wedge guard still fires, and *fast*: a guest
    //     silent past its budget is caught the moment it passes, at 154 s, not
    //     held to the 300 s backstop.
    let Some(wedged) = ceiling_verdict(None, TIGHT + Duration::from_secs(1), TIGHT, GUEST_QUIET, 40)
    else {
        return Err(String::from(
            "a guest silent past its budget was not caught — the liveness guard cannot fire",
        ));
    };
    if !wedged.starts_with(STALLED) {
        return Err(format!("a genuine wedge past the budget stopped reading as one: {wedged}"));
    }
    // (d) Idle-safety, the property the no-speaker boots demand: a guest silent
    //     for 90 s — inside the 102 s a healthy idle machine with no periodic
    //     speaker was measured at — but still *within* its budget is not a wedge.
    if ceiling_verdict(None, Duration::from_secs(100), TIGHT, Duration::from_secs(90), 40).is_some()
    {
        return Err(String::from(
            "a guest idle-but-within-budget was called wedged — a boot with no periodic speaker \
             would red healthy",
        ));
    }

    // 3c. **The other side of `ceiling.max(GUEST_WEDGED)`**: a ceiling *above*
    //     `GUEST_WEDGED` must itself be the backstop, not get clamped down to
    //     the floor. `CEILING` (380 s, from case 1) is such a ceiling; a guest
    //     talking past `GUEST_WEDGED` (300 s) but still short of `CEILING` is
    //     not yet at its backstop and must run on.
    if ceiling_verdict(None, GUEST_WEDGED + Duration::from_secs(50), CEILING, talking, 40).is_some()
    {
        return Err(String::from(
            "a guest talking past GUEST_WEDGED but short of a higher ceiling was ended anyway — \
             the backstop did not follow a ceiling above GUEST_WEDGED",
        ));
    }

    // 4. **What the verdict carries, which is the half that was missing.** Every
    //    arm above names a death in one sentence; until 2026-08-18 that sentence
    //    was the whole of what a failure arm had, and a `DOUBLE FAULT on CPU 1`
    //    went into the record with its report — written, on IST1, 6688 bytes of
    //    it — never printed. Both directions, because the second is what keeps a
    //    stall or a slow test from pasting a boot's console at somebody.
    const DF: &str = "[kernel 6.204 cpu1] DOUBLE FAULT on CPU 1 (pid=Some(Pid(2)) tid=Some(Tid(0)))";
    let window_before = "[kernel 6.201 cpu0] spawn: /system/bin/test_rs_console_line_atomicity pid=41\n";
    let window_serial = format!(
        "AAAAAAAA\n{DF}\n\
         [kernel 6.204 cpu1]   cr2=0xffff800002672ff8 (address that caused the fault chain)\n\
         [kernel 6.204 cpu1]   rip=0xffffffff80121a40  rsp=0xffff800002673000  rbp=0x0\n"
    );
    let died_verdict = ceiling_verdict(Some(DF), early, CEILING, quiet, 40)
        .ok_or("a staged double fault reached no verdict at all")?;
    let carried = WaitVerdict::new(died_verdict.clone(), &[window_before, &window_serial]);
    for want in [DIED_SAYING, "cr2=0xffff800002672ff8", "rip=0xffffffff80121a40"] {
        if !carried.to_string().contains(want) {
            return Err(format!(
                "the verdict names the death and drops {want:?}, which is the defect \
                 `issues/kernel/a-double-fault-on-cpu-1-under-a-wide-suite.md` is \
                 about:\n{carried}"
            ));
        }
    }
    if sentence(&carried) != died_verdict {
        return Err(format!(
            "the report changed the sentence a summary quotes:\n{}\n{died_verdict}",
            sentence(&carried)
        ));
    }
    // The other direction. A guest still talking at its ceiling has nothing to
    // account for, and a verdict that grew a serial log would be a second defect
    // dressed as a fix.
    let quiet_capture = WaitVerdict::new(slow.clone(), &["[kernel 0.377 cpu0] NVMe: found\n"]);
    if quiet_capture.to_string() != slow {
        return Err(format!(
            "a verdict on a capture nothing died in grew a report:\n{quiet_capture}"
        ));
    }
    // And the capture being handed over at all is the argument, not a habit: an
    // empty slice is what a wait with nothing to show says, and it says it.
    if WaitVerdict::new(died_verdict.clone(), &[]).to_string() != died_verdict {
        return Err(String::from("a verdict built on no capture invented a report"));
    }

    // 5. **The pre-marker death, the other half of that omission.** A test that
    //    never announced itself has an empty `serial`, so the arm formatting
    //    `serial` prints nothing and `before` is the only record there is —
    //    `sched_check_build`'s empty `serial:` block in run `31890991692`. Both
    //    directions, because a started test's window is already where its arm
    //    looks.
    let never = WaitVerdict::for_test(slow.clone(), window_before, "", false);
    if !never.to_string().contains(NEVER_ANNOUNCED)
        || !never.to_string().contains("console_line_atomicity")
    {
        return Err(format!(
            "a test that never announced itself kept its sentence and dropped the only window \
             there was:\n{never}"
        ));
    }
    if sentence(&never) != slow {
        return Err(format!(
            "the window changed the sentence a summary quotes:\n{}\n{slow}",
            sentence(&never)
        ));
    }
    let announced = WaitVerdict::for_test(slow.clone(), window_before, "AAAA\n", true);
    if announced.to_string() != slow {
        return Err(format!(
            "a test that did announce itself grew the window before it:\n{announced}"
        ));
    }

    eprintln!(
        "  [ceiling] the panic, the stall, the slow test and the healthy run, each named apart \
         from the other three; the panic's verdict carries the kernel's own {} lines, a test \
         that never announced itself carries the {} it was given, and the rest carry nothing",
        carried.to_string().lines().count() - 1,
        never.to_string().lines().count() - 2,
    );
    Ok(())
}

fn sentence(verdict: &WaitVerdict) -> String {
    verdict.to_string().lines().next().unwrap_or_default().to_string()
}
