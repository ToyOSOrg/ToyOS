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
  ToyOS job list runs them. A run's value is its p50, and the exact test
  over four runs each resolves complete separation at p = 2/70; three of one
  cannot reach 0.05.
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
  difference.
- **A limit the machine reaches is marked, not hidden.** A settled row
  carries the clock settled before it (`mhz_settled`) and after it
  (`mhz_after`). Where the second is under nine tenths of the first, the
  measurement ran into a power or heat limit. The AMD bench's package
  limit held a long single-thread row at 1.4 to 1.9 GHz after a 4.29 GHz
  start. The verdict counts those runs per system. No program reads the
  package's limit state or temperature on ToyOS, so the clock is what both
  systems report.

## Linux makes the requests ToyOS makes

**The CPU's frequency.** Linux runs under the frequency request ToyOS's
kernel makes of the same CPU, so the clock is the hardware's answer to one
request on both systems:

- **The T14** (i5-1135G7, `intel_pstate` with HWP). ToyOS asks for HWP's
  lowest to highest performance at EPP 0x80 (`kernel/pure/cpu.rs`). Ubuntu's
  default, `powersave` at `balance_performance` (EPP 0x80), is that request.
  Leave it, and check `governor=powersave*8`, `epp=balance_performance*8`,
  `intel_no_turbo=0`.
- **The AMD bench** (Ryzen 7 4800U, `acpi-cpufreq`). ToyOS writes no P-state
  or boost register, so the firmware's P0 and Core Performance Boost stand.
  Linux's `performance` governor asks for that. Set it before the Linux runs
  and give back `schedutil` after them:

  ```text
  echo performance | sudo tee /sys/devices/system/cpu/cpufreq/policy*/scaling_governor
  echo schedutil | sudo tee /sys/devices/system/cpu/cpufreq/policy*/scaling_governor
  ```

  Check `governor=performance*16` and `boost=1`.

When ToyOS's request changes, this section changes with it.

**The clock.** Every timestamp is read from the TSC on ToyOS, so Linux runs
with `clocksource=tsc`. A clocksource the vDSO cannot read costs every
`Instant::now` a syscall: on 2026-10-24 the AMD bench's Linux took 1.21 µs a
read (`time.now`) against the T14's 14 ns, and every per-operation timing
there carried it.

## Both write the bench's NVMe

`persist` and `fs.disk` run on each system's persistent filesystem on the
bench's NVMe. On Linux that is the runner's home, ext4 on the bench's root
partition. On ToyOS it is DATA: the bench's TOYOS-DATA partition, its fourth,
named `ToyOS data`. It is served through the DATA file server and the NVMe
driver. The partition holds a ToyOS volume only once its first block carries
the designation stamp. Until then the file server writes nothing to it and
keeps DATA in memory, and the row refuses the boot by name. The first boot
after the stamp formats the volume, so it is not a measured run. Every row's
`served_by` names the filesystem and the disk it measured.

## What is not a comparison

The table also holds rows only one system can measure. They show the
system's figure beside the other's reason: `unsupported` and `unmeasured`
rows, Linux's `.slack1ns` variants and `fair.session`, and ToyOS's
`tail.diag`. They are records, not verdicts.
