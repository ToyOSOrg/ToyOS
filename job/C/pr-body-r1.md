Closes three issues, one commit each, so they can be split.

## 1. `irq_census` compares no two lines by their place in the capture (`30a01bd5`)

Closes `issues/the-irq-census-judge-reds-on-two-exits-stamped-in-the-other-order.md`.

**The defect.** The judge behind the T14 row `irq_census_conservation` (`tests/toyos.rs`) read the capture as if its lines were in the order their counters were read. They are not. An exit reads the counters before `log::emit` stamps its line, two exits on two CPUs run side by side, and `log::read::drain_ordered` merges the shards by stamp. So two of its checks could red with no kernel defect: the per-source monotonic check, and both issuer checks on `tlb: shootdowns=` ("went backwards", and the last `tlb:` line under a newer `irq:` line's `tlb` count).

**Why the ordered checks are deleted, not repaired.** A line's read has an upper bound, its stamp. It has no lower bound: the exit can be held for any time between the read and the stamp, and a kernel record carries no process id that would tie the line to its exit. So no pair of lines from two exits has a known read order, and any comparison between them can red a correct kernel. Any set of snapshots is consistent with monotonic counters read at some times before their stamps. The judge now compares none:

- a CPU's census is the largest count each source reached on any of that CPU's lines, which is what the newest read would show whatever the stamp order;
- the issuer's count is the largest `shootdowns=`. That bound is still sound. An exit reads its deliveries before the issuer's total (`process.rs`, `tlb::log_census`), and whichever exit first swaps a total into `REPORTED` logs it, so every total an exit read is on some `tlb:` line;
- the device-delivery check (no AP has a device count) and the delivery ≤ issued check run on those largest counts, unchanged.

The deleted monotonic check guarded against a counter word two CPUs write. A capture cannot show that. The writer, one `add qword ptr gs:[slot], 1` into the CPU's own block, is visible in the code.

**Moved, not changed:** `control_regs`'s doc comment had been left above `irq_census`, so rustdoc read it as the census judge's doc. It is moved back above `control_regs`, word for word.

**Test.** `tests/checks.rs`'s `irq_census_verdict`, a host test, feeds the judge two exits stamped in the other order from their reads. Both of the issue's cases are in one capture: X's cpu1 line (`kick=5`) is stamped after Y's (`kick=6`), and X logs `shootdowns=9` before Y logs `shootdowns=7`, with cpu2 and cpu3 at 9 deliveries. The judge stays green on it. The test also holds the judge red on an AP device delivery on a line that is not that AP's last, on a delivery past the largest issued count, and on a capture with no `tlb:` line.

**Negative control.** The test was run against the base judge (`origin/main`'s `irq_census`, the whole of this commit's `tests/toyos.rs` reverted), and against two narrower controls that each remove the base's earlier ordered checks. It reds on each of the issue's three paths in turn:

| arm | exit | the red |
|---|---|---|
| this head | 0 | — |
| `irq-census-1-base-judge.patch` | 101 | `cpu1's \`kick\` count went backwards` |
| `irq-census-2-base-judge-without-its-monotonic-check.patch` | 101 | `the issuer census went backwards: [9, 7]` |
| `irq-census-3-base-judge-without-either-ordered-check.patch` | 101 | `cpu2 took 9 tlb IPI(s) against 7 counted issue(s)` |

Each patch was applied after `git apply --check`, run, and reversed in the same script (`run-irq-census-controls.sh`). The tree was clean afterwards. All three patches are posted as comments.

**Filed off this fence:** `common::irqcensus::observe`, the suite summary's reader, also keeps a CPU's last stamped line, and the module's header says that line is the whole census: `issues/the-irq-census-summary-takes-a-cpus-last-stamped-line-as-its-newest-read.md`.

## 2. The LAPIC selftests' "took interrupts after it" can fail (`86244ac8`)

Closes `issues/the-spurious-and-unclaimed-selftests-took-interrupts-after-check-cannot-fail.md`. Kernel code, under `boot-actuators` only.

**The defect.** `spurious::selftest` and `unclaimed::selftest` read `deliveries_total(cpu)` before `apic::send_self`, and that total also counts the probe. Once `delivered` held, "total > taken_before" was true at the first poll. A CPU the probe's handler left deaf still printed `3/3 … and the CPU took interrupts after it`.

**The fix.** Each selftest reads the total once the handler has returned. The selftest polls on the CPU that the self-IPI interrupts, so when `delivered` holds, the handler has already run to its `iretq`. It then waits for an interrupt past that total. Waiting alone would not be enough: nothing else promises the BSP an interrupt within the 50 ms budget. By reading the code, the selftests run in the device phase, before any scheduler pass has armed a one-shot, and `apic::init_timer` leaves the timer stopped. The only writers of `TIMER_INIT` are `OneShot::arm`, `stop_timer` and the timer entry's re-arm. The old check never found this out because the probe alone satisfied it. So each selftest arms the one-shot with `arm_within(QUANTUM_NS)`, as `usb_gate` and the deadline's wedge do for a CPU that has to take a timer interrupt. Any source still counts. The Ring 0 fire re-arms one quantum on, so the BSP of an armed image ticks every quantum from there to its first pass. That is the state an executing CPU is in, and `do_preempt` treats the `need_resched` it sets as moot before a CPU has a thread. `boot.rs`'s note on `interrupt_selftests` claimed the timer was already ticking. It now says what the selftests need: interrupts on and the timer calibrated.

**Mutation (the exit's), posted as comments:**

- `selftests-deaf-after-probe-on-head.patch`: both gates `iretq` with IF cleared in the returned RFLAGS (`and qword ptr [rsp + 16], -513`), so the CPU takes no interrupt once the probe's handler returns. Each selftest turns interrupts back on once its verdict is read, so the second selftest and the boot can go on.
- `selftests-deaf-after-probe-on-base.patch`: the same mutation on the base. It reverts this commit's kernel diff, which leaves the files byte-identical to `origin/main`'s, then applies the mutation. This is the negative control.

Both were applied after `git apply --check`, then linted (`cargo clippy --target x86_64-unknown-none --features boot-actuators`, `--ci host`'s shape) and built (`cargo build`, same target and features) as the kernel, with exit 0 each. Each was reversed in the same script (`build-selftest-mutations.sh`), and the tree was clean afterwards. The built kernels disassemble to `and QWORD PTR [rsp+0x10],0xfffffffffffffdff` directly before each entry's `iretq`.

**What I have not seen, and who can.** No QEMU guest test arms `lapic-spurious-selftest` or `unclaimed-vector-selftest`. Their only reader is the T14 row `lapic_spurious_vector`, on the `selftests` boot (`SELFTESTS` in `tests/toyos.rs`). CI's guest suite compiles this code (its kernels carry `boot-actuators`) but runs neither selftest. This host has neither the ToyOS toolchain nor the declared QEMU, so I could not stage the images (`--metal-readback`) either. The exit needs three T14 runs of `lapic_spurious_vector`:

1. this head: green, both selftests `3/3` with `(0 -> 1)`;
2. this head plus `selftests-deaf-after-probe-on-head.patch`: red, with both `LAPIC: spurious selftest FAILED — cpu0 took no interrupt at all after the spurious one` and the unclaimed counterpart;
3. this head plus `selftests-deaf-after-probe-on-base.patch`, the negative control: green, which is the defect, a deaf CPU passing.

Arm 1 also shows whether the armed one-shot changes anything else on that boot.

## 3. The console's routine lines go on stdout (`71fb985c`)

Closes `issues/the-console-says-its-routine-lines-as-errors.md`.

`/system/bin/console` now writes `console: ready …`, `console: keyboard layout is now …`, `console: client N has the keyboard until it exits` and `console: client N gave the keyboard back` with `println!` (an Info record). Before, they were on stderr (an Error record, drawn red). This is what 6e6a8f71 (#733) did for the compositor's and the terminal's identical lines. `console: no framebuffer, exiting` and `console: dropping client N — …` stay on stderr, because each one reports a failure. The console is ToyOS-only (`exempt.owns`), so the host gate does not compile it; CI's guest suite build does. No harness boot runs `console/system.toml`, so no guest test reaches these lines. Reading the diff covers it: four `eprintln!` become `println!`, which takes the same arguments.

## Closing the issues

Each issue file is deleted in its own commit. `git grep` at the base for each slug, and for each title, found no citation outside the file itself. That search covers `.github/`. Durable rules went to their sites: the stamp order is in `irq_census`'s doc comment; the probe is already in the total, in the comment at each selftest's read. The console's rule was already on `Stream::severity`.

## Gates

Logs are in the job directory's `C/` (`host.log`, and `logs/`).

| gate | at | exit |
|---|---|---|
| `cargo run -- --ci host`, run by `gate.sh` as the non-root user, tree clean | `71fb985c` | **0**, "Host: 75 step(s), all green" (`C/gate2/host.log`). An earlier attempt at the same head exited 101 before any step, on a root-owned temp root another task's killed gate left in `/tmp`; not a verdict on this branch (`C/host.log`). |
| `cargo test --test toyos-checks irq_census_verdict`, then the three census controls above | `71fb985c` | 0, 101, 101, 101 |
| kernel clippy and build, `boot-actuators`: head, and each selftest mutation | `71fb985c` | 0, 0, 0 |

Not run here: the guest suite and every T14 row (see 2 above). The census judge's own row, `irq_census_conservation`, would read the new judge over a real T14 capture. The issue's exit needs only the host test, but the row is the judge's one real input.

## What I am unsure of

- The judge's tlb bound covers exit lines only. A blocked-task dump and `SYS_SHUTDOWN` print `irq:` lines with no `tlb:` read after them. On `main`, a shutdown census was already excluded because it falls after `logkeeper`'s flush, and this branch does not change that exposure.
- That the BSP's one-shot is stopped when the selftests run is my reading of the code, not a measurement. If something does arm it, the selftest's `arm_within` only shortens it.

## Size

`git diff --shortstat origin/main...HEAD`: 10 files, +146 −146.

- Production: kernel +11 −5, console +4 −4.
- Tests: `tests/toyos.rs` +51 −59, of which 20 lines are the moved `control_regs` doc. `tests/checks.rs` +58.
- Issues: +22 −78.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_015tYBoMwh9xUcBBLG35wTFr
