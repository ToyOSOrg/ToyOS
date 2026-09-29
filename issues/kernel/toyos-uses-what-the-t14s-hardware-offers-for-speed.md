---
status: open
kind: track
opened: 2026-09-29
---

# ToyOS uses what the T14's hardware offers for speed

Owner ruling: ToyOS takes the hardware features that make it faster, at least
on the T14; what the T14 lacks is
`issues/hardware/newer-hardware-offers-what-the-t14-lacks.md`. A figure is the
median of 11 T14 runs with its spread, taken by P0's program. A stage is done
at or past P0's Linux figure, or with the gap filed as a defect carrying both;
one Linux has no counterpart for lands only if it beats ToyOS without it. The
kernel uses no vector registers, as Linux's kernel does not.

- **Pin — a test holds a thread on a named CPU**, a `test-actuators`
  `SYS_DEBUG` action. Exit: a guest test pins a thread to each CPU in turn and
  reads CPUID.1:EBX[31:24] across 1000 yields at each: one APIC ID per pin, a
  different one per CPU. Mutation: a pin the scheduler's placement ignores.
  Oracle: the CPU's own APIC ID.
- **P0 — The Linux baseline.** One Rust program, built for
  `x86_64-unknown-linux-gnu` and for ToyOS, measures memcpy from 64 B to 64
  MiB; AES-128-GCM, ChaCha20-Poly1305 and SHA-256 at sshd's RustCrypto
  versions; pipe round trips, and throughput at 64 B to 64 KiB; munmaps per
  second of a page a sibling on another CPU touched; lateness past a 1 ms
  timer; NVMe reads, read-only; an idle minute's package energy; and the
  T14's PCI, USB, NVMe and cpuidle identity. Exit: the outputs committed as
  fixtures. Mutation: the munmap pair on one CPU, as `sched_getcpu` or Pin
  reads it, is refused. Oracle: Linux.
- **P1 — XSAVE, and AVX to AVX-512 in user programs**, with one save path on
  every CPU, XSAVEOPT: TCG implements neither XSAVEC nor XSAVES
  (`target/i386/cpu.c:1012-1014` at QEMU v11.1.1), and the security track's
  S11 switches its CET state by `wrmsr`, as `fs_base` is. Exit: threads that
  fill their vector registers differently each find their own after a switch,
  under TCG and on the T14; P0's ChaCha20-Poly1305 figure. Mutation: a save
  mask short of Hi16_ZMM, and under TCG of YMM. Oracle: TCG and the T14 on SDM
  Vol. 1 §13.
- **P2 — Kernel copies with `rep movsb`**, `+ermsb` for the kernel and the
  `core`, `alloc` and `compiler_builtins` it builds; a CPU without ERMS is
  refused by name. Exit: a gate finds `rep movsb` in `kernel.elf`'s `memcpy`;
  P0's pipe figures. Mutation: no `+ermsb`. Oracle: P0's Linux figures.
- **P3 — 1 GiB direct-map leaves** where a GiB holds one memory type. Exit: a
  `toyos-bootmap` host test gives a GiB holding one UC range 2 MiB leaves and
  a uniform GiB one leaf; P0's 64 KiB pipe figure and the boot's leaf counts.
  Mutation: a 1 GiB leaf for every GiB. Oracle: SDM Vol. 3A §14.11.9.
- **P4 — Shootdowns only to the CPUs that ran the space**, after Pin. Exit: a
  sibling pinned on another CPU touches a page the initiator unmaps and then
  faults; a case in `kernel-loom/tests/tlb_shootdown.rs` switches a CPU into
  the space while the initiator reads the target set; P0's munmap figure.
  Mutation: a target set that omits a CPU the space ran on, and a CPU that
  joins the set after loading CR3. Oracle: loom.
- **P5 — FSGSBASE, measured, then taken or rejected**; it costs a `swapgs`
  entry and the security track's S4 barriers. Exit: P0's round trips with and
  without, a loss becoming a `rejected` issue with both figures; taken, an NMI
  that arrives while a user GS base is set finds the kernel's. Mutation: an
  IST entry that decides `swapgs` from the saved CS alone. Oracle: Linux's
  `paranoid_entry`.
- **P6 — Deep idle** from `_CST`, as Linux takes the T14's states, after the
  AML interpreter `issues/kernel/the-scheduler-and-the-clocks-never-talk.md`
  waits on. Exit: P0's idle minute's energy, and P0's timer lateness no worse
  than `hlt`'s by more than the chosen state's exit latency. Mutation: always
  the deepest state. Oracle: P0's Linux figures.
- **P7 — NVMe APST and host memory buffer**, after blockd drives the T14's
  drive (step 3 of
  `issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`); as
  Linux, APST stays within `default_ps_max_latency_us` and honours
  `NVME_QUIRK_NO_APST` and `NVME_QUIRK_NO_DEEPEST_PS`. Exit: P0's block
  figures, and P6's idle minute with the drive idle. Mutation: a host test's
  table that admits a state past the bound. Oracle: Linux's
  `nvme_configure_apst` over P0's `id-ctrl`.
