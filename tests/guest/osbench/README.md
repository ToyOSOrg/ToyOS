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
  states each system's. Each row's verdict says whether the runs resolve a
  difference. Each p is its row's alone: with no difference anywhere, about
  2.9% of rows resolve by chance, a few in a table this size.
- **The clocks are told beside each verdict.** No program reads the
  package's limit state or temperature on ToyOS, so the clock is what both
  systems report. A settled row carries the clock settled before it
  (`mhz_settled`) and after it (`mhz_after`). The verdict counts per system
  the runs where the second is under nine tenths of the first. A power or
  heat limit reached inside the row is the likely cause, an inference: the
  AMD bench's package limit held a long single-thread row at 1.4 to 1.9 GHz
  after a 4.29 GHz start. A row that began already limited is not marked
  that way, since both its settles read the limited clock. So the verdict
  also says where the systems' median settled clocks lie more than a tenth
  apart. A settle that ran out its 0.5 s says so (`unsettled`), and is
  counted rather than judged.

## Linux makes the requests ToyOS makes

**The CPU's frequency.** Linux runs under the frequency request ToyOS's
kernel makes of the same CPU, so the clock is the hardware's answer to one
request on both systems:

- **The T14** (i5-1135G7, `intel_pstate` with HWP). ToyOS asks for HWP's
  lowest to highest performance at EPP 0x80 (`kernel/pure/cpu.rs`). Ubuntu's
  default `powersave` governor makes HWP's request with the EPP its
  `energy_performance_preference` holds, so write 0x80 there before each
  Linux run, which `intel_pstate` takes as a number:

  ```text
  echo 128 | sudo tee /sys/devices/system/cpu/cpufreq/policy*/energy_performance_preference
  ```

  Check `governor=powersave*8` and `intel_no_turbo=0`; `epp` reads 128 by
  the name the driver gives it, `balance_performance` where that is 0x80 on
  this CPU. Linux's lowest request is `cpuinfo_min_freq`, which need not be
  HWP's lowest, ToyOS's: that reaches a CPU waking from idle, not a settled
  row.
- **The AMD bench** (Ryzen 7 4800U, `acpi-cpufreq`). ToyOS writes no P-state
  or boost register, so the firmware's P0 and Core Performance Boost stand.
  Linux's `performance` governor asks for that. Set it before each Linux
  run, since every ToyOS boot between them boots Linux back to `schedutil`,
  and give `schedutil` back after the last:

  ```text
  echo performance | sudo tee /sys/devices/system/cpu/cpufreq/policy*/scaling_governor
  echo schedutil | sudo tee /sys/devices/system/cpu/cpufreq/policy*/scaling_governor
  ```

  Check `governor=performance*16` and `boost=1`.

When ToyOS's request changes, this section changes with it.

**The groups.** `fair.apart` sets each workload in a group of its own: a
ToyOS Job, and on Linux a scope's cgroup, which is a group only where the
user's manager delegates the `cpu` controller. Check
`fair.apart.units=3xcgroup`. With `1xcgroup` Linux's `apart` is
thread-fair, and the pair compares nothing.

**The clock.** Every timestamp is read from the TSC on ToyOS, so Linux runs
with `clocksource=tsc`. A clocksource the vDSO cannot read costs every
`Instant::now` a syscall: on 2026-10-24 the AMD bench's Linux took 1.21 µs a
read (`time.now`) against the T14's 14 ns, and every per-operation timing
there carried it.

## The disk is not yet one disk

`persist` writes each system's persistent filesystem. On Linux that is the
runner's home, ext4 on the bench's NVMe. On ToyOS it is still the boot
stick's LOG volume, FAT32 over USB 2.0, because ToyOS's DATA is the bench's
partition only once ToyOS has designated it, and ToyOS cannot yet do that
itself. So `persist` compares two media. Its lines name theirs, and
`osbench-compare` refuses a verdict on any row whose media differ. The
`osbench` row refuses the boot that formats a newly designated DATA as a
run, and says how long the format took.

## What is not a comparison

The table also holds rows only one system can measure. They show the
system's figure beside the other's reason: `unsupported` and `unmeasured`
rows, Linux's `.slack1ns` variants and `fair.session`, and ToyOS's
`tail.diag`. They are records, not verdicts.
