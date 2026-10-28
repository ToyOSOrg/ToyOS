# osbench: ToyOS and Linux on one bench

`osbench` is one program built for both systems (`src/main.rs`). A comparison
is one bench's runs of each, read together by `osbench-compare`. What follows
is what makes those runs comparable. A run taken any other way measures the
setup, not the system.

## Build both halves from one revision

The ToyOS half is the `osbench` metal row's image (`tests/images/benchcase.toml`):

```text
cargo test --test toyos-build -- --metal --metal-readback <dir> osbench
```

The Linux half is static (`x86_64-unknown-linux-musl`, linked by the
toolchain's own LLD per `.cargo/config.toml`), so a bench runs it without a
toolchain:

```text
rustup target add --toolchain nightly-2026-07-22 x86_64-unknown-linux-musl
cargo +nightly-2026-07-22 build -p osbench --target x86_64-unknown-linux-musl --profile toyos
```

That nightly is the build's stage-0 compiler (`build/sysroot.rs`'s
`STAGE0_CARGO`), the upstream one nearest ToyOS's own. With the `toyos`
profile osbench's own code is generated alike for both; std's platform layer
and the C library beneath it are each system's own.

## Run

- **At least four runs of each system, interleaved**: Linux, ToyOS, Linux,
  ToyOS. A ToyOS run is one boot and one readback directory. A Linux run is
  `osbench all </dev/null` over ssh, each group a process of its own as the
  ToyOS job list runs them. A run's value is its p50, compared by the exact
  two-sided Mann-Whitney test at 0.05. With four runs each the only p under
  0.05 is 2/70, so a resolved row is one whose every run of one system beat
  every run of the other. Fewer runs of either system than four cannot reach
  that, except against more of the other: three against five can (2/56).
  Four a side is where a row can first resolve, a starting sample and no
  promise of precision: a row whose runs overlap is unresolved, not equal.
  Boots of one image differ: the AMD bench's `crunch.single.deflate` read
  63.0 to 63.6 ms on some boots and 66.6 to 66.8 ms on others (2026-10-27),
  so no single boot stands for its system.
- **The machine on AC power and otherwise idle**, for both.
- **A ToyOS run counts once its readback passes the row**:
  `cargo test --test toyos-build -- --metal --metal-readback <dir> osbench`,
  with `--metal-host` for a bench other than the T14.
- **Compare** every run of the bench in one call:
  `cargo run -p osbench-compare -- <linux runs> <toyos kernel.logs>`. It
  refuses runs of one system that disagree on their setup (`SETUP`) and
  states each system's, and what each run's boot chose (`PER_RUN`, the
  clocksource) run by run. Each row's verdict says whether the runs resolve a
  difference. Each p is its row's alone: with no difference anywhere, about
  2.9% of rows resolve by chance, a few in a table this size.
- **The clocks are told beside each verdict.** No program reads the
  package's limit state or temperature on ToyOS, so the clock is what both
  systems report. A clock reading is the difference between a short add
  chain's time and a long one's, so what reading the time costs is no part
  of it; a settle holds once every reading for 50 ms agrees within 3%. A
  settled row carries the clock settled before it
  (`mhz_settled`) and after it (`mhz_after`). The verdict counts per system
  the runs where the second is under nine tenths of the first. A power or
  heat limit reached inside the row is the likely cause, an inference: the
  AMD bench's package limit held a long single-thread row at 1.4 to 1.9 GHz
  after a 4.29 GHz start. A row that began already limited is not marked
  that way, since both its settles read the limited clock. So the verdict
  also says where the systems' median settled clocks lie more than a tenth
  apart. A settle that ran out its 0.5 s says so (`unsettled`), and is
  counted rather than judged.

## Linux as installed, and Linux under ToyOS's requests

**The baseline is Linux as the bench boots it**: its own governor, its own
clocksource, its own checks. What it does on that machine is ordinary OS
behaviour and part of what is compared. A comparison names its
configuration in its setup lines, and `osbench-compare` refuses to mix two.
What a boot chooses for itself, the clocksource, belongs to Linux as booted:
each run states its own, and runs that differ are compared together.

**A matched request is a controlled experiment, labelled as one.** It asks
the CPU's frequency control for what ToyOS's kernel asks of the same CPU, so
the clock is the hardware's answer to one request on both systems. It
answers a different question from the baseline, and never replaces it:

- **The T14** (i5-1135G7, `intel_pstate` with HWP) has no experiment: its
  baseline already makes ToyOS's request but for the floor. ToyOS asks for
  HWP's lowest to highest performance at EPP 0x80 (`kernel/pure/cpu.rs`).
  The baseline's `IA32_HWP_REQUEST` read `0x80002a04` on every CPU
  (2026-10-28): EPP 0x80 (`epp=balance_performance*8`), highest 0x2a, and
  lowest 4 where ToyOS asks 1. The floor reaches a CPU waking from idle, not
  a settled row. Check `governor=powersave*8`, `epp=balance_performance*8`
  and `intel_no_turbo=0`.
- **The AMD bench** (Ryzen 7 4800U, `acpi-cpufreq`). ToyOS writes no P-state
  or boost register, so the firmware's P0 and Core Performance Boost stand.
  Linux's `performance` governor asks for that. The experiment sets it
  before each Linux run, since every ToyOS boot between them boots Linux
  back to `schedutil`, and gives `schedutil` back after the last:

  ```text
  echo performance | sudo tee /sys/devices/system/cpu/cpufreq/policy*/scaling_governor
  echo schedutil | sudo tee /sys/devices/system/cpu/cpufreq/policy*/scaling_governor
  ```

  Check `governor=performance*16` and `boost=1`.

When ToyOS's request changes, the experiment changes with it.

**The groups.** `fair.apart` sets each workload in a group of its own: a
ToyOS Job, and on Linux a scope's cgroup, which is a group only where the
user's manager delegates the `cpu` controller. Check
`fair.apart.units=3xcgroup`. With `1xcgroup` Linux's `apart` is
thread-fair, and the pair compares nothing.

**The clock.** Every timestamp is read from the TSC on ToyOS. The AMD
bench's Linux chooses its clocksource at each boot. Its watchdog moves it
off the TSC when, early in boot, the TSC and the HPET disagree. On the HPET
each `Instant::now` is a syscall's cost, 1.21 µs against the T14's 14 ns,
carried by every per-operation timing of that run. In 30 boots' kernel logs
(2026-10-28) the watchdog fired in 17:
- in 12, Linux's refined calibration read 1799.54 to 1799.61 MHz, some
  1,630 ppm above the TSC's 1796.62 MHz;
- in 5, with a right calibration, the TSC ran some 1.7 ms ahead of the HPET
  over half a second, a step whose cause is unknown.

In a 13th boot the calibration read 1799.565 MHz and the watchdog let it
stand, so that Linux kept a TSC clock some 1,630 ppm fast for the whole
boot. Its `clocksource=tsc` reads the same as a right one's. The 12 others
kept the TSC on a right calibration. Each run's clocksource is part of the
Linux it measured, so the compare states it per run.

At runtime neither fault shows: across 16 CPUs and idle gaps up to 1 s, no
CPU's TSC read behind another's and no step exceeded the 1.2 µs of a read.
A Linux forced onto the TSC (`tsc=reliable`) has those checks switched off,
so it is no baseline.

## Energy: its own boot, its own Linux run

`energy` is a group by name, outside `all`: idle power is the whole
machine's, so nothing else runs beside it. A ToyOS run is a boot of the
`osbench_energy` metal row, the benchcase image with that one job:

```text
cargo test --test toyos-build -- --metal --metal-readback <dir> osbench_energy
```

A Linux run reads the same registers through the `msr` driver, as root:

```text
sudo modprobe msr
sudo osbench energy </dev/null
```

Both read the package's energy counter in its own unit, its C2, C3, C6 and
C7 residency where Intel documents them (not PC8 to PC10 on the T14, which
SDM Vol. 4 gives no Tiger Lake), and each CPU's APERF/MPERF and core
residency, and on AMD each core's energy (`cores_mw`). The group first waits,
up to 120 s, for three seconds in a row in which the machine ran under 10 ms
of CPU time (`energy.quiet`): a ToyOS boot reaches it while its services,
the ACPI server's idle states among them, are still starting.

The rows: `energy.idle` (60 s), `energy.wake_10ms` (one thread sleeping 10 ms
over and over, 30 s), `energy.spin.t1` and `energy.spin.all` (one thread and
one per CPU running the compute chunk, 30 s each), each a second's power
(`.package`, mW), residency (`.pc2` to `.pc7`, permille) and under a spin a
chunk's energy (`.per_chunk`, nJ). No unit is better higher or lower by
itself: a spin that ran faster may draw more, and work is not slowed for
power. ToyOS's every read kicks every CPU once a second; Linux's reads the
package on CPU 0, where the reader is pinned, and each CPU only at a load's
two ends.

**The same devices, on both**:

- **The panel lit.** ToyOS has no display driver: the firmware's GOP
  scan-out stays on, and the kernel log's `GOP:` line names it. Linux keeps
  its panel on and its console unblanked; `energy.display` and
  `energy.backlight` say what it was.
- **The radios off.** ToyOS drives no Wi-Fi or Bluetooth, so Linux blocks
  both for the run: `sudo rfkill block all`, and on the AMD bench, whose
  only link is its Wi-Fi, from a unit that outlives the session:

  ```text
  sudo systemd-run --unit=osbench-energy sh -c 'rfkill block all; /tmp/osbench energy </dev/null >/tmp/energy.txt; rfkill unblock all'
  ```

  `energy.radios` and `energy.links` say what each was. On the T14 both
  systems keep the I219's link up.
- **The machine on AC, and the same stick or none in it.**

## The disk is not yet one disk

`persist` writes each system's persistent filesystem. On Linux that is the
runner's home, ext4 on the bench's NVMe. On ToyOS it is still the boot
stick's LOG volume, FAT32 over USB 2.0, because ToyOS's DATA is the bench's
partition only once ToyOS has designated it, and ToyOS cannot yet do that
itself. So `persist` compares two media. Its lines name theirs, and
`osbench-compare` refuses a verdict on any row whose media differ.

## What is not a comparison

The table also holds rows only one system can measure. They show the
system's figure beside the other's reason: `unsupported` and `unmeasured`
rows, Linux's `.slack1ns` variants and `fair.session`, and ToyOS's
`tail.diag`. They are records, not verdicts.
