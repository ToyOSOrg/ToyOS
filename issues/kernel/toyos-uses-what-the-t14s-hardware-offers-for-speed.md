---
status: open
kind: track
opened: 2026-09-29
---

# ToyOS uses what the T14's hardware offers for speed

Owner ruling: ToyOS takes the newest hardware features that make it faster, at
least on the T14, an i5-1135G7, Tiger Lake, family 6 model 0x8C
(`issues/hardware/the-t14-touchpad-is-i2c-hid-and-unbuilt.md`). Features the
T14 lacks are `issues/hardware/newer-hardware-offers-what-the-t14-lacks.md`.
Intel documents most of these for the Tiger Lake family, not for the
i5-1135G7, so a stage's first step confirms its bit in S0's CPUID capture
(`issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md`), and
a bit S0 finds clear closes the stage. Linux `path:line`s are read at that
track's tag, `Ubuntu-6.8.0-142.142`; Intel's documents are the SDM
325462-093US and the 11th-generation datasheet 631121-012.

**How a stage is judged.** Each figure is the median of 11 runs on the T14,
with its spread, taken by P0's program. A stage is done when ToyOS's figure is
at or past P0's Linux figure, or when the gap is filed as a defect with both
numbers. A stage Linux has no counterpart for is measured against ToyOS
without it and lands only if it wins.

**Owned elsewhere, not repeated here.** HWP, RAPL and turbo: PR #590's
`issues/kernel/the-kernel-owns-cpu-performance-state.md`. Global kernel pages:
`issues/kernel/page-global-is-a-decision-nobody-has-made.md`. A vector per CPU
or per queue: `issues/kernel/every-interrupt-lands-on-the-boot-cpu.md`.
Placement that weighs caches and idle depth:
`issues/kernel/placement-is-blind-to-caches-and-topology.md` and
`issues/kernel/the-scheduler-and-the-clocks-never-talk.md`. 4 KiB process
pages: `issues/kernel/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`.
NVMe with commands in flight: step 3 of
`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`. The
stick at SuperSpeed:
`issues/hardware/the-t14s-superspeed-stick-enumerates-at-high-speed-under-toyos.md`.
PCID is in use, and a switch keeps the incoming space's translations
(`CR3_NOFLUSH`, `kernel/src/arch/x86_64/paging.rs`).

## Stages

- **P0 — The Linux baseline.** One Rust program, built from one source for
  `x86_64-unknown-linux-gnu` under Ubuntu on the T14 and for ToyOS, measures:
  memcpy at 64 B, 4 KiB, 256 KiB and 64 MiB; AES-128-GCM, ChaCha20-Poly1305
  and SHA-256 through the RustCrypto versions sshd links
  (`userland/Cargo.lock`); a pipe round trip between two threads and between
  two processes, and pipe throughput at 64 B, 4 KiB and 64 KiB writes; munmaps
  per second of a page a sibling thread on another CPU has touched; a
  sleeping thread's lateness past a 1 ms timer; 4 KiB random reads at 1 and 32
  in flight and 1 MiB sequential reads of the raw NVMe namespace, read-only;
  and an idle minute's package energy, from
  `/sys/class/powercap/intel-rapl:0/energy_uj` under Linux. It runs in S0's
  session with `lspci -vvv -nn`, `lsusb -tv`, `nvme id-ctrl -H /dev/nvme0`,
  `nvme get-feature -f 0x0c -H /dev/nvme0` (APST) and `-f 0x06` (volatile
  write cache), and `grep .
  /sys/devices/system/cpu/cpu0/cpuidle/state*/{name,desc,latency,residency}`.
  **Exit**: the outputs committed as fixtures; S0's wipe waits on that commit.
- **P1 — XSAVE, and with it AVX, AVX2 and AVX-512 in user programs.**
  `CR4.OSXSAVE` is clear and a thread's saved state is `FXSAVE64`'s
  (`kernel/src/arch/x86_64/fpu.rs`), so no user program may run a VEX or EVEX
  instruction. RustCrypto's `cpufeatures` 0.2.17 gates every AVX feature on
  `OSXSAVE` and `XCR0` (`src/x86.rs:67-83,97-101`), so `chacha20` 0.9.1's AVX2
  backend (`src/lib.rs:173`) and `sha2` 0.10.9's AVX2 SHA-512
  (`src/sha512/x86.rs:14`) never run; AES-NI, PCLMULQDQ and SHA-NI need no
  `XCR0` and run today (`cpufeatures` `src/x86.rs:116,122,142`). First,
  AVX-512 from S0's capture: the datasheet warns it may be absent on some
  SKUs (§2.4.10). `CR4.OSXSAVE` and `XCR0` (x87, SSE and AVX, and opmask,
  ZMM_Hi256 and Hi16_ZMM where CPUID offers them, SDM Vol. 1 §13.1, §13.3)
  join the one declaration and are asserted on every CPU. A switch saves with
  XSAVES where CPUID.(0xD,1) offers it, which the security track's S11 needs
  for CET_U, and with XSAVEOPT where not: TCG offers neither XSAVEC nor XSAVES
  (`target/i386/cpu.c:1012-1014` at QEMU v11.1.1) and no AVX-512
  (`:977-985`). A thread's state grows from 512 bytes to CPUID.(0xD,0):EBX.
  `aes` 0.8.4 and `polyval` 0.6.2 carry no VAES or VPCLMULQDQ path, so those
  wait on a crate version that has one. **Exit**, T14: P0's
  ChaCha20-Poly1305 figure; two threads that fill their vector registers with
  different patterns and switch each find their own, and a save mask short of
  Hi16_ZMM reds it.
- **P2 — Kernel copies with `rep movsb`.** compiler-builtins copies with
  `rep movsb` only under `target_feature = "ermsb"`
  (`rust/library/compiler-builtins/compiler-builtins/src/mem/x86_64.rs:23-54`
  at the `rust/` commit, 1b236638), which the kernel's `x86_64-unknown-none`
  build lacks, so every kernel copy is `rep movsq` and a byte tail. First,
  CPUID.(7,0):EBX bit 9 (ERMS) and EDX bit 4 (FSRM) from S0
  (`arch/x86/include/asm/cpufeatures.h:251,418`); SDM Vol. 1 Table 5-2
  introduces fast short `rep movsb` with Ice Lake and names no Tiger Lake
  part. `+ermsb` for the kernel and the `core`, `alloc` and
  `compiler_builtins` it builds makes ERMS a boot requirement, refused by name,
  as the security track's S4 makes SMAP one. **Exit**, T14: P0's pipe figures
  before and after.
- **P3 — 1 GiB leaves in the direct map.** Every direct-map leaf is 2 MiB
  (`toyos-bootmap/src/lib.rs`, `PAGE_2M`), with a page directory per GiB
  (`issues/kernel/the-direct-maps-page-directories-come-from-a-512-kib-heap.md`).
  First, CPUID.80000001H:EDX bit 26 (`cpufeatures.h:67`); TCG offers it
  (`target/i386/cpu.c:945`). A GiB of one memory type takes one 1 GiB leaf,
  and a GiB of mixed types keeps 2 MiB leaves (SDM Vol. 3A §14.11.9).
  **Exit**, T14: P0's 64 KiB pipe throughput before and after, and the boot's
  count of 1 GiB and 2 MiB direct-map leaves.
- **P4 — Shootdowns only where the space ran.** Every shootdown interrupts
  every other CPU, and each flushes every PCID (`kernel/src/arch/x86_64/tlb.rs`
  header, `flush_tlb_all` in `paging.rs`). Linux interrupts only the CPUs in
  the space's `mm_cpumask`, which a switch keeps (`arch/x86/mm/tlb.c:575-628`),
  and flushes by page up to 33 pages (`tlb.c:1003,1048-1076`). A shootdown
  goes to the CPUs that ran the space since its last flush and flushes that
  space's PCID, or its pages. This is memory management: a guest test's
  sibling thread on another CPU touches a page, the initiator unmaps it, and
  the sibling's next touch faults; a target set that omits a CPU the space
  ran on lets the read through and reds it, and
  `kernel-loom/tests/tlb_shootdown.rs` models the ordering. **Exit**, T14:
  P0's munmap figure and the `tlb:` census line's shootdowns and wait.
- **P5 — FSGSBASE, measured, then taken or rejected.** Every switch reads and
  writes `IA32_FS_BASE` with `rdmsr` and `wrmsr`
  (`kernel/src/arch/x86_64/hw.rs:423,445`). One `CR4` bit enables all four
  base instructions (SDM Vol. 3A §2.5) in ring 3 as in ring 0, and `GS.base`
  is the kernel's per-CPU pointer with no `swapgs` on entry, which is why
  `control_regs.rs` forbids it. Taking it means a `swapgs` entry, and then the
  security track's S4 owes Linux's swapgs barriers
  (`arch/x86/kernel/cpu/bugs.c:943-944`). It buys `rdfsbase`/`wrfsbase` on
  the switch and a user thread that sets its own FS base. **Exit**, T14: P0's
  round trips with the `wrfsbase` switch, the `swapgs` entry and the
  barriers, against without; a loss becomes a `rejected` issue carrying both
  figures.
- **P6 — Deep idle.** An idle CPU runs `sti; hlt`
  (`kernel/src/arch/x86_64/hw.rs:52-53`), which reaches C1 or C1E (datasheet
  §3.2.3); Tiger Lake cores and the package go to C10 through MWAIT hints
  (§3.2.4-3.2.5). TIGERLAKE_L has no table in `intel_idle`
  (`drivers/idle/intel_idle.c:1603-1657`), so Linux takes its hints from ACPI
  `_CST` (`:2347-2349`), which ToyOS cannot evaluate without an AML
  interpreter (`issues/kernel/the-scheduler-and-the-clocks-never-talk.md`).
  P0's cpuidle capture becomes a TIGERLAKE_L table, as `intel_idle` carries
  one per model, and an idle CPU picks the deepest state whose target
  residency fits before its next deadline. Package C9 and C10 need every PCIe
  link in L1.2 (datasheet §3.7, Table 3-8), which is P7's. **Exit**, T14: the
  idle desktop minute's package energy, read by the sampler of stage 4 of PR
  #590's track, against P0's; P0's timer lateness no worse than `hlt`'s by
  more than the chosen state's exit latency.
- **P7 — The T14's NVMe drive, idle and busy.** blockd runs several I/O queue
  pairs on the claim's one MSI-X vector (`userland/blockd/src/nvme.rs`). The
  drive's model is unrecorded; P0's `id-ctrl` says whether it offers APST
  (`apsta`), a host memory buffer (`hmpre`) and a volatile write cache. What
  it offers is taken, as Linux takes it: APST through feature 0x0C
  (`drivers/nvme/host/core.c:2495-2592`, `include/linux/nvme.h:1222`), a host
  memory buffer through 0x0D (`drivers/nvme/host/pci.c:2108`). Blocked on
  blockd driving the T14's NVMe, which step 3 of the small-kernel track meets
  on the T14 only with its step 10. **Exit**, T14: P0's block figures against
  Linux's on the same drive, and P6's idle minute with the drive idle.

The whole-system figure is PR #568's T14 LLVM build bar once it lands; each
stage names a narrower one.

**Not taken.** Vector registers in the kernel: its target is soft-float, as
Linux builds its kernel's C and Rust without SSE or AVX
(`arch/x86/Makefile:70-71`), and the kernel has no bulk data to encrypt or
hash.
