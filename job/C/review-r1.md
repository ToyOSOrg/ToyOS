## Review, round 1, head `71fb985c`

Read against `origin/main` (`f260e0b9`, the merge base): `git log origin/main..71fb985c` (3 commits, one per issue), `git diff origin/main...71fb985c`, and every changed file whole at the head.

Net lines (`git diff --shortstat origin/main...71fb985c`): 10 files, +146 -146. Production: kernel +11 -5, console +4 -4. Tests: `tests/toyos.rs` +51 -59 (20 of each are the `control_regs` doc moved), `tests/checks.rs` +58. Issues: +22 -78. The production growth is two reads moved and two `arm_within` calls, all under `boot-actuators`. I accept it.

Evidence checked:
- `C/gate2/host.{exit,head,log}`: `EXIT=0`, head `71fb985c84c5…`, log ends `Host: 75 step(s), all green`. Every `FAILED` in the log falls inside a `[ci] control` step that reports `verdict(s) reached`. `checks::irq_census_verdict ... ok` is at log line 770.
- `C/logs/controls-irq.out`: the head exits 0, and the three census controls exit 101 each. `control-irq-census-1-base-judge.log` reds on `cpu1's kick count went backwards`, which is the issue's first path. The tree was clean after the restore.
- `C/logs/selftest-builds.out`: kernel clippy and build pass for the head and for both selftest mutations. The head mutation clears IF in the `iretq` frame of both gates and turns interrupts back on after the verdict. That is the defect the exit names.

I traced the judge's soundness through the code. A CPU's counters are monotonic, so taking the largest count per source is the same as reading the newest line, whatever the stamp order. For the issuer bound, every value swapped into `REPORTED` is logged by whoever swapped it in. Removing the "monotonic" and "issuer went backwards" checks drops no guard a capture could hold: a pair of lines from two exits has no known read order. The judge's mutations are held. `max` changed to `min` on the per-CPU fold reds the AP-device refusal, because the device count is on a line that is not that CPU's last. `max` changed to last-line or `min` on `issued` reds the good capture, because cpu2 has 9 deliveries against 7 issued.

For the selftest fix: `delivered` is read on the CPU the self-IPI interrupts, so the probe's `took` has already counted and returned before `taken` is read. `arm_within` only arms or shortens the timer. A Ring 0 fire re-arms one quantum on, polls the boot deadline, and sets a `need_resched` that means nothing before the scheduler runs. The 50 ms `settles` budget covers a 10 ms one-shot. The new state (the BSP ticking through the rest of the device phase) is reached only with the `lapic-spurious-selftest` / `unclaimed-vector-selftest` actuator set.

### Owed (unrun on this host; not a BLOCKER at this round; each claim resting on them is marked unrun in the body)
- `guest / suite` green at `71fb985c`, once the PR is readied. It is the only gate that compiles `userland/console` (an `exempt` app) for ToyOS.
- T14 `lapic_spurious_vector`, the three arms the body names. Arm 1 (head): green, `3/3` with `(0 -> 1)` for both selftests. Arm 2 (head + `selftests-deaf-after-probe-on-head.patch`): red, on both `… took no interrupt at all after …` lines. Arm 3 (head + `selftests-deaf-after-probe-on-base.patch`): green. Arm 3 is the negative control this high-risk interrupt change owes. Arms 2 and 3 together are the issue's exit.
- T14: every other `SELFTESTS` row (`pci_capability_walk`, `read_fault_selftests`, `leak_rollback_selftest`, `xhci_xecp_walk`, `xhci_descriptor_walk`, `sysret_ss_reload`, `input_merge`, `operation_nesting`, and the rest that share the image), read from arm 1's boot. They share that one boot, which now has the BSP's one-shot armed from the selftests on.
- T14 `irq_census_conservation` at the head: the changed judge over a real capture.

### BLOCKER
None open at this round. The owed T14 arms above become BLOCKERs at the round that judges the close of `issues/the-spurious-and-unclaimed-selftests-took-interrupts-after-check-cannot-fail.md` if they are still absent.

### NOTE
- `kernel/src/arch/x86_64/idt/spurious.rs` / `kernel/src/arch/x86_64/idt/unclaimed.rs` (commit `86244ac8`) — the commit deletes `issues/the-spurious-and-unclaimed-selftests-took-interrupts-after-check-cannot-fail.md`, but that issue's exit (the `selftests` row reds on the mutation) is unmet until arms 2 and 3 above are read — so either those arms are read before landing, or the deletion is dropped from the branch and the code lands with the issue still open; a close whose exit is unmet does not land.
- `tests/toyos.rs:2693` — the largest-count-per-source fold is written inline in `irq_census`. `tests/common/irqcensus.rs` is the module that exists for that census's two readers, and the issue this branch files (`issues/the-irq-census-summary-takes-a-cpus-last-stamped-line-as-its-newest-read.md`) needs the same fold for `observe`. Put the fold in `common::irqcensus` (one method on `Census`), so the judge and the later fix read one declaration and the module does not grow a sibling.
- PR body, section 2: "Arm 1 also shows whether the armed one-shot changes anything else on that boot" names only `lapic_spurious_vector`'s judge. The rows that read the rest of that boot are the other `SELFTESTS` rows, and the body should list them as owed by name.

LAND AFTER NAMED CHANGES
