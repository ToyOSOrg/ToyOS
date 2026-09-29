---
status: open
kind: track
opened: 2026-09-26
---

# ToyOS runs on ARM64: QEMU `virt`, EL1 only

Supersedes `8a277bb2^:issues/kernel/arm64-is-a-decision-nobody-has-made.md`, which is
folded in below and deleted. It keeps that file's settled points: compile-time
dispatch (one `cfg_attr(path)` module choice, no `dyn Arch`, no `Kernel<A>`,
~20 distinct symbols behind ~145 `crate::arch::` paths), a 4 KiB granule with
2 MiB L2 blocks, and the driver boundary of
`issues/kernel/a-driver-is-tested-on-the-host-and-its-real-implementation-is-one-instruction-deep.md`
as a precondition for the drivers it names.

**Purpose** (owner ruling, 2026-09-29): ARM is for quick runs on the
development Mac under QEMU with HVF, and for keeping the kernel's
architecture clean by being its second one. ToyOS's arm64 target is QEMU
`virt` on that Mac; no ARM hardware is a target and no work goes into one,
because ARM hardware is too proprietary today.

**Motivation, measured on this development machine (an M4 Pro):**
`qemu-system-aarch64 -M virt,gic-version=3 -accel hvf -cpu host -smp 4` reaches
edk2-stable202408's UEFI shell on the PL011 in under 8 s, native under HVF.
Every x86 guest on this host runs under TCG emulation instead — there is no
`-accel hvf` path for `qemu-system-x86_64` here.

## Owner rulings, 2026-09-26

- **Parked** (owner ruling, 2026-09-29). No later stage starts now. The Exit
  (LLVM compile measurement) starts only after the track's stages are complete;
  until then it is unmet.
- **Hardware discovery is ACPI only** (edk2 MADT, GTDT and SPCR under QEMU); no devicetree.
- **Start.** Stage 0 (shared groundwork on x86) and stages 1-3 (toolchain,
  loader, kernel reaching serial on QEMU `virt`) start now. Stage 0 waits for
  small-kernel stage 2 (PR #513, "One way to wait") to land before its own
  changes land — both touch the same files. Stage 4 onward waits for
  small-kernel stage 6 (`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`),
  so the per-CPU IRQ relay and the i8042 plumbing are not ported and then
  deleted.
- **EL1 only.** The kernel runs at EL1, dropping from EL2 when entered there.
  No VHE-at-EL2 kernel.
- **The memory-model audit is stage 0's**: the kernel's `Relaxed` orderings
  and `Mmio`'s barrier semantics are real latent defects on x86 too
  (`aaddf38a^:issues/kernel/the-stops-no-lost-wake-claim-rests-on-x86-locked-rmws.md`
  is one instance).
- **C on AArch64 goes through clang**, whose driver knows
  `aarch64-unknown-toyos` (`issues/build/toyos-builds-itself.md`); doomgeneric
  compiles for it. Doom and the C corpus stay x86-only until userland runs on
  ARM.
- **Randomness** comes from RNDR where the CPU has it, and from virtio-rng
  under QEMU/HVF, behind one `sys_random` source.
- **TLS**: each architecture uses its ABI's variant (x86-64 keeps variant II;
  AArch64 uses variant I with TLSDESC), and the loader handles both.
- **The CI runner for the aarch64 tier** is decided at stage 8, after
  measuring whether hosted `ubuntu-24.04-arm` exposes `/dev/kvm` and whether
  HVF is usable inside a hosted `macos-latest` runner's VM.

## Measured, on `main` at `03b1b4db`

**Size.**

| What | Lines |
|---|---|
| Kernel, all of `kernel/src/**/*.rs` | 62,931 |
| `kernel/src/arch/` | 8,773 |
| &nbsp;&nbsp;└ `arch/syscall/` (arch-neutral but sits inside `arch/`) | 3,459 |
| &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;└ `arch/syscall/gate.rs`, the only x86 part of it (34 lines name x86 registers; the other 11 files name none) | 184 |
| &nbsp;&nbsp;└ the rest (idt/, apic, percpu, smp, tlb, fpu, cpu, control_regs, mtrr, pat, entry) | 5,314 |

**Wholly x86 subsystems outside `arch/`, 4,614 lines:** `drivers/i8042/` 1,490,
`iommu/vtd/` 2,419, `drivers/ioapic.rs` 323, `drivers/watchdog.rs` (Intel TCO)
160, `rtc.rs` (CMOS through port I/O) 222. Also `drivers/serial.rs` (433
lines: a 16550 at a port, `serial.rs:28-39,157-165,389-396`).

**x86-only pure crates, 1,305 lines:** `toyos-ps2` 377, `toyos-tco` 313,
`toyos-pcid` 388 (it becomes an ASID allocator), `toyos-bootmap` 227
(PML4[0]/PML4[256], `toyos-bootmap/src/lib.rs:21-29`).

**Inline assembly**, 834 lines by a paren-depth scan of
`asm!`/`naked_asm!`/`global_asm!` bodies (undercounts: macro bodies like
`switch_save!` are not counted). 626 lines are in `arch/`. Outside it:
`loader/start.rs` 47, `sched/driver.rs` 20 (plus the `switch_*` macros at
`sched/driver.rs:1019-1062`), `irq_census.rs` 19, `nmi_gate.rs` 16, `main.rs`
13, `drivers/serial.rs` 13, `hw.rs` 10, `blackbox.rs` 6, `iommu/vtd/table.rs`
6, `panic_console/mod.rs:1294` 1, `xhci/wait/mod.rs:36` 1. Userland plus
`toyos-abi`: 54 lines (`libc/memory.rs` 23, `toyos-abi/src/syscall.rs` 20). The
rust fork's std has two naked-asm sites: `_start`
(`rust/library/std/src/sys/pal/toyos/mod.rs:44`) and `__tls_get_addr` reading
`fs:[8]` (`pal/toyos/tls.rs:43`).

**Coupling.** 45 non-arch kernel files reference `arch::`; 145
`crate::arch::` paths name about 100 distinct `module::symbol` names (an upper
bound). Heaviest: `arch::cpu` (57 references), `arch::percpu` (28),
`arch::apic` (14), `arch::tlb` (11), `arch::smp` (10).

**What assumes x86 in particular:**

- **Port I/O** (`cpu::inb/outb/inw/outw`): `rtc.rs:203-204`,
  `drivers/serial.rs` (16 sites), `drivers/i8042/mod.rs` (12 sites),
  `drivers/acpi.rs:325,345` (reset and PM1a soft-off),
  `drivers/watchdog.rs:84-158`, `bootloader/src/watchdog.rs:195,204`.
- **APIC / IOAPIC / MSI.** `hw.rs:1-5`: "Everything here is x2APIC, TSC or a
  single instruction." The MSI doorbell `0xFEE0_0000` is hardcoded three
  times: `drivers/pci.rs:31`, `iommu/vtd/interrupt.rs:61`,
  `iommu/vtd/fault.rs:30`. ACPI decodes exactly APIC, FACP, HPET, MCFG, DMAR
  (`drivers/acpi.rs:108-111`); ARM needs MADT GICC/GICD/ITS, GTDT, SPCR,
  IORT, PPTT.
- **i8042.** 18 kernel files mention it, including the scheduler
  (`scheduler.rs`, `sched/driver.rs`), `heartbeat.rs`, `irq_ring.rs`,
  `log/console.rs`.
- **TSC.** 112 mentions in 11 non-arch files (`clock.rs`, `deadline.rs`,
  `hardlockup/`, `panic_reboot.rs`, `xhci/`, `sched/dump.rs`). The bootloader
  calls `_rdtsc`/`__cpuid` and reads MSR 0x3B at
  `bootloader/src/main.rs:583-598`.
- **CPU-state declaration.** `arch/control_regs.rs:1-6` is CR0/CR4/EFER;
  ARM's equivalent is SCTLR_EL1/TCR_EL1/MAIR_EL1/CPACR_EL1 (and HCR_EL2 when
  entered at EL2). CLAUDE.md's one-declaration rule transfers intact; the
  file does not.
- **Paging.** `mm/paging.rs:23-37` is the x86 PTE format (PAT bit 12, NX bit
  63, one root); `mm/paging.rs:1-7` states invalidation is INVPCID/INVLPG.
  ARM splits kernel (TTBR1) from user (TTBR0), carries AttrIndx→MAIR,
  AP[2:1], UXN/PXN, and ASIDs in the TTBR.
- **TLS.** The kernel builds x86 variant II (`loader/tls.rs:1`); std's
  `__tls_get_addr` uses `fs:`. AArch64 is variant I; LLVM emits TLSDESC by
  default.
- **Calling conventions.** `extern "sysv64"` at `main.rs:205`,
  `deadline.rs:194`, `sched/driver.rs:892`, `i8042/mod.rs:490`,
  `bootloader/src/main.rs:785`.
- **Entropy.** `hasher.rs:37` asserts RDRAND; HVF exposes no RNDR.
- **Boot protocol.** `toyos-abi/src/boot.rs:3-20` carries `rsdp_addr` and
  `boot_pml4_addr`, no DTB field.
- **Memory ordering.** 980 explicit orderings in the kernel, 711 `Relaxed`,
  plus 17 `fence(` calls. `mm/mmio.rs:14` justifies `Sync` by saying volatile
  accesses "order correctly regardless of which CPU issues them" — true under
  x86 TSO and UC memory, false on ARM: a descriptor written to normal memory
  and then a Device-memory doorbell need a `dmb`/`dsb` between them.

**Build and toolchain.** 57 hardcoded `x86_64-unknown-{toyos,none,uefi}`
mentions across 11 `src/` files. `qemu-system-x86_64` is spawned in 9 places
across `src/` and `tests/`. The harness hardwires q35
(`tests/common/qemu.rs:4246-4266`). `kvm_usable()` is
`cfg!(target_arch = "x86_64") && …` (`src/lib.rs:65`). The rust fork has only
`x86_64_unknown_toyos.rs`; its base `base/toyos.rs` is arch-neutral.
`toyos-ld` already parses and applies AArch64 relocations for its Mach-O host
output (`collect.rs` 80 `Aarch64` mentions, `reloc.rs` 69) but hardwires
`EM_X86_64` (`emit_elf.rs:1021`), `R_X86_64_RELATIVE`
(`emit_elf.rs:1228,1313,1399`) and `IMAGE_FILE_MACHINE_AMD64`
(`emit_pe.rs:143`) on output, and has no AArch64 TLS relocations. `toyos-elf`
refuses anything but `EM_X86_64` (`toyos-elf/src/header.rs:24,75`).

**Already abstracted.** The syscall stub already has both arms
(`toyos-abi/src/syscall.rs:678,703`: `syscall` and `svc #0`). `toyos-sched`
(8,099 lines) is pure behind `Machine`/`Hw`
(`toyos-sched/src/hw.rs:88-158`: `now`, `set_timer`, `stop_timer`,
`irq_guard`, `halt`, `need_resched`, `switch`), with `kernel/src/arch/x86_64/hw.rs` as
the one x86 implementation and a simulator as the other. PCI is
ECAM/MMIO-only (`drivers/pci.rs:134-154`), no `0xCF8`. NVMe, xHCI and virtio
have no ISA dependence beyond TSC-based waits. The bootloader is the `uefi`
crate (0.26, aarch64-capable already); only 12 of its 2,881 lines are x86.
The pure decision crates carry no arch at all: `toyos-acpi`, `-gpt`,
`-fat32`, `-dma`, `-userbound`, `-proclife`, `-blackbox`, `-elide` and
others.

**The biggest porting costs, in order:** interrupt delivery (IDT, APIC,
IOAPIC, MSI, VT-d remapping, NMI → GICv3 redistributors/ITS, SGIs as IPIs,
pseudo-NMI; the widest seam, and small-kernel stage 6 shrinks it); the IOMMU
(`iommu/vtd/`, 2,419 lines, → SMMUv3 through IORT — a whole new driver, and
userland drivers cannot run without it); paging and TLB (the x86 PTE format
and PCID become TTBR0/TTBR1, MAIR, ASIDs; ARM's broadcast `TLBI …IS` makes
most of `arch/tlb.rs`'s 303-line IPI shootdown machinery unnecessary — a
contract change, not a port); the memory model (the 711 `Relaxed` orderings
and every doorbell-after-descriptor site — undiscovered TSO reliance is the
one cost nobody can estimate from a grep); userland TLS and the toolchain
(variant I and TLSDESC across `toyos-ld`, the kernel loader and std); the test harness (33,907 lines in
`tests/common/`, written against q35, i8042, OVMF, KVM, `intel-iommu`).

## Sharing, not forking

Each of these keeps a single copy that both arches use, and each can land
before any aarch64 file exists, with x86 as its only user:

- **One `Arch` value** in the build system and the harness, replacing the 57
  hardcoded triple mentions.
- **Move arch-neutral code out of `arch/`**: the 3,275 syscall lines, and
  `hw.rs`'s policy-free parts — `arch/` should shrink to what differs.
- **One seam per concept**, each with an x86 implementation today: interrupt
  masking (`IrqGuard`/`LogCommitGuard`), the clock (TSC → `clock::now`), the
  deadline timer, the per-CPU base (`gs:` / `TPIDR_EL1`), the context switch,
  the MSI doorbell, machine-wide TLB invalidation (IPI shootdown on x86,
  `TLBI IS` on ARM), the console UART (16550 port vs. SPCR-placed MMIO UART),
  reset and power-off (ACPI PM1a vs. PSCI).
- **A barrier contract on `Mmio`**: a `write` orders prior normal-memory
  writes before the device sees it, as Linux's `writel` does — free on x86,
  required on ARM. Doorbells become explicit.
- **ACPI decoding stays in `toyos-acpi`** for both arches (MADT's LAPIC and
  GICC entries, GTDT, SPCR, IORT, PPTT), so the kernel reads one description
  language.
- **Pure crates stay arch-free.** A per-arch decision that becomes pure lives
  in its own crate, as `toyos-bootmap` does: it grows a TTBR plan, and
  `toyos-pcid` becomes an ASID/PCID allocator.

## x86 left in generic code

Each is its own issue, owned by the stage that removes it:

- `issues/kernel/the-saved-kernel-context-names-x86-registers.md` (stage 4)
- `issues/kernel/msi-and-pin-routing-take-an-x86-vector-and-apic-id.md` (stage 4)
- `issues/kernel/the-boot-timing-handoff-is-named-for-the-tsc.md` (stage 4)
- `issues/kernel/the-crash-evidence-records-x86-fault-registers.md` (stage 5)
- `issues/kernel/the-aarch64-kernel-builds-with-dead-code-allowed.md` (stage 7)

## Stages

Each stage names its exit; "measured" means a number from a run.

0. **Rulings and shared seams, before any aarch64 file exists.** `src/` gains
   an `Arch`, replacing the 57 triple mentions. `arch/syscall/` minus
   `gate.rs` moves out of `arch/`. The MSI doorbell becomes
   one arch-provided constant. `IrqGuard`/`LogCommitGuard` become one arch
   primitive. The loom model owed by
   `aaddf38a^:issues/kernel/the-stops-no-lost-wake-claim-rests-on-x86-locked-rmws.md`
   lands, and the `Mmio` barrier contract above is written and asserted.
   **Exit**: x86 builds and passes unchanged. `rg 'x86_64-unknown' src/`
   names one `Arch` table.

1. **`aarch64-unknown-toyos` in the rust fork.** A target spec:
   `aarch64-unknown-none-elf`, PIC, frame pointers. Std pal `_start` and TLS
   for variant I. **Exit**: `cargo +toyos build --target aarch64-unknown-toyos`
   builds `std` plus a hello-world and the whole Rust userland.

2. **The UEFI loader on AArch64** (`aarch64-unknown-uefi`, tier 2 upstream,
   no fork work needed). Entry is `extern "C"` rather than `sysv64`.
   `toyos-bootmap` grows a TTBR0/TTBR1 plan. The loader's TSC/CPUID/MSR and
   TCO lines become per-arch. `KernelArgs` carries what the arch needs.
   **Exit**: under `qemu-system-aarch64 -M virt` with edk2, the loader reads ROOT through
   firmware block I/O (small-kernel stage 1), exits boot services, and jumps
   to a kernel stub that writes one line to the PL011 SPCR names.

3. **The kernel reaches the serial console on `virt`.**
   `kernel/src/arch/aarch64/`: exception vectors, drop from EL2 to EL1 if
   entered at EL2, and the SCTLR/TCR/MAIR declaration applied and asserted on
   every CPU. The PL011 console, placed by SPCR. `toyos-acpi` decodes MADT
   GIC entries, GTDT, SPCR and MCFG. **Exit**: the kernel boots, prints the
   memory map and the ACPI tables it read on the PL011, and a deliberate
   panic renders on the serial port and the GOP panel.

4. **GIC, timer, MMU and exception levels.** Waits for small-kernel stage 6.
   GICv3 distributor, redistributor and ITS. The generic timer's CNTV/CNTP
   as the clock and the deadline, replacing TSC+HPET. The TTBR0/TTBR1 split
   with ASIDs in place of PCID. Synchronous-exception decoding of user
   faults into the existing fault paths, and `svc` into the dispatcher.
   **Exit**: a user process takes a page fault and a syscall on one CPU; the
   timer drives preemption; an interrupt storm test ends with no lost timer
   tick; the longest interrupts-off window is measured against x86's.
   The entry's EL2 writes of `CNTHCTL_EL2`, `CNTVOFF_EL2` and `CPTR_EL2`
   (`kernel/src/arch/aarch64/boot.rs`) are untested until here: deleting any
   one stays green in `virt_el2_drop`, because stage 3 reads no counter and
   runs no FP. This stage's timer and FP tests run under that EL2 profile too,
   and each of the three deletions is shown red.

5. **SMP through PSCI.** `CPU_ON` from MADT GICC entries, SGIs as the IPI,
   broadcast TLBI behind the machine-wide invalidation contract. **Exit**:
   `-smp 8` boots all CPUs; every CPU asserts the control-register
   declaration; the shootdown stress test and the loom-checked stop both
   pass; `CPU_OFF`/`SYSTEM_RESET`/`SYSTEM_OFF` replace ACPI reset and PM1a.
   The TLS-descriptor resolver lands here, in std with its loader half and a
   `dlopen` test; until it does the kernel refuses `R_AARCH64_TLSDESC` by name
   (`toyos_elf::rela::ExeRefusal::TlsDescriptor` for an executable,
   `toyos_elf::RelocError::TlsDescriptor` for a library).

6. **Virtio on `virt`.** virtio-pci (ECAM from MCFG) for blk, net, gpu,
   sound, input and rng. virtio-input replaces the i8042 as the
   key-transition source. virtio-rng, or SMCCC TRNG, feeds `sys_random`
   alongside RNDR. SMMUv3 is on `virt` (`-M virt,iommu=smmuv3`), decoded from
   IORT. **Exit**: netd claims its NIC through an SMMUv3 domain; a
   foreign-DMA test faults into a `DMA FAULT` record, not a crash.
   Stage 0's three DMA-ordering fixes (NVMe's phase before its body, xHCI
   `TrbRing::put`'s body before its cycle bit, and the event ring's cycle bit
   before its body) get a test here that reds with `put` written back as one
   `self.buf.write(off, trb)`; x86's TSO hides all three from every guest
   test until then.

7. **Userland boots.** `init`, `logd`, the compositor, netd, soundd and sshd,
   built for `aarch64-unknown-toyos`. C programs stay x86-only until this
   stage runs one. **Exit**: the desktop comes up on virtio-gpu; `ssh`
   works from the host; `/log` survives a reboot; the same `system.toml`
   drives both arches.

8. **An aarch64 tier in the harness.** `tests/common/qemu.rs` takes an
   `Arch`: `virt`, edk2-aarch64, HVF on Apple hosts (TCG otherwise).
   `src/tiers.rs` gains the arch axis.
   The CI runner is picked here, after measuring hosted `macos-latest`
   (whether HVF is usable inside the runner VM) and hosted
   `ubuntu-24.04-arm` (whether `/dev/kvm` exists there). **Exit**: the fast
   tier runs on aarch64 locally on the M4 host; the nightly runs the aarch64
   tier on whatever runner that measurement picks; a test red on only one
   arch is a named known-red, not a skip.

## Exit

ARM is done when LLVM compiles on ToyOS arm64. The work starts only after the
track's stages are complete. Then LLVM is compiled once
natively on the development Mac and once in a Linux arm64 guest (a distro known
to be fast) under QEMU with HVF, to get the floor; then once on ToyOS arm64
under QEMU with HVF, with the same source and build settings. A single data
point each is enough. It passes when ToyOS is at least as fast as the Linux
guest, and the numbers are recorded in this file. This is a one-time manual
measurement, not an automated test, so the rule that timing verdicts come only
from metal does not apply to it. ToyOS compiling LLVM is
`issues/build/toyos-builds-itself.md`'s work on AArch64.

## Interactions with other tracks

- **Small kernel**
  (`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`):
  its stage 1 (the loader loads ROOT into memory) removes every kernel
  storage driver from the ARM bring-up path — land it before stage 0 here.
  Its stage 3 (storage in userland) removes NVMe, USB storage and the block
  layer from the port entirely. Its stage 6 (handlers only post to a
  `Watch`) deletes the per-CPU IRQ relay and the driver list in the
  scheduler pass, which the i8042 currently threads through
  (`scheduler.rs`, `sched/driver.rs`) — stage 4 here follows it.
- **The driver boundary**
  (`issues/kernel/a-driver-is-tested-on-the-host-and-its-real-implementation-is-one-instruction-deep.md`)
  is the stated precondition: its four traits (registers, clock, DMA,
  interrupt arrival) are the arch seam for drivers, and ARM is the second
  implementation that makes them real boundaries.
- **Page size**
  (`issues/kernel/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`):
  2 MiB L2 blocks map identically on both arches, so no change is forced by
  this track.
