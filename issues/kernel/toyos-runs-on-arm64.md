---
status: open
kind: track
opened: 2026-09-26
---

# ToyOS runs on ARM64: QEMU `virt` first, ACPI only, EL1 only

Supersedes `issues/kernel/arm64-is-a-decision-nobody-has-made.md`, which is
folded in below and deleted. It keeps that file's settled points: compile-time
dispatch (one `cfg_attr(path)` module choice, no `dyn Arch`, no `Kernel<A>`,
~20 distinct symbols behind ~145 `crate::arch::` paths), a 4 KiB granule with
2 MiB L2 blocks, and the driver boundary of
`issues/kernel/a-driver-is-tested-on-the-host-and-its-real-implementation-is-one-instruction-deep.md`
as a precondition for the drivers it names.

**Motivation, measured on this development machine (an M4 Pro):**
`qemu-system-aarch64 -M virt,gic-version=3 -accel hvf -cpu host -smp 4` reaches
edk2-stable202408's UEFI shell on the PL011 in under 8 s, native under HVF.
Every x86 guest on this host runs under TCG emulation instead — there is no
`-accel hvf` path for `qemu-system-x86_64` here.

## Owner rulings, 2026-09-26

- **Start.** Stage 0 (shared groundwork on x86) and stages 1-3 (toolchain,
  loader, kernel reaching serial on QEMU `virt`) start now. Stage 0 waits for
  small-kernel stage 2 (PR #513, "One way to wait") to land before its own
  changes land — both touch the same files. Stage 4 onward waits for
  small-kernel stage 6 (`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`),
  so the per-CPU IRQ relay and the i8042 plumbing are not ported and then
  deleted.
- **ACPI only.** No device tree path. This closes every Snapdragon laptop,
  whose Linux support boots from DT.
- **EL1 only.** The kernel runs at EL1, dropping from EL2 when entered there.
  No VHE-at-EL2 kernel.
- **`tcblaunch.exe` is not an allowed exception** to the dependency rule's "no
  binary outside Rust and QEMU". Reaching EL2 on a Snapdragon laptop needs
  Qualcomm's Secure Launch running Microsoft's signed `tcblaunch.exe` on the
  CPU (`slbounce`); without EL2, ToyOS does not own the PCIe SMMU there
  (`x1-el2.dtso`: "this IOMMU is controlled by the firmware" while under
  Gunyah). So no Snapdragon laptop can host userland drivers, and none is a
  target for now.
- **The memory-model audit is stage 0's**, not discovered on real ARM
  hardware: the kernel's `Relaxed` orderings and `Mmio`'s barrier semantics are
  real latent defects on x86 too (`issues/kernel/the-stops-no-lost-wake-claim-rests-on-x86-locked-rmws.md`
  is one instance).
- **`toyos-cc` gets no AArch64 backend yet.** Doom and tinycc stay x86-only
  until userland runs on ARM, and that is decided again then.
- **Randomness** comes from RNDR where the CPU has it, and from virtio-rng
  under QEMU/HVF, behind one `sys_random` source.
- **TLS**: each architecture uses its ABI's variant (x86-64 keeps variant II;
  AArch64 uses variant I with TLSDESC), and the loader handles both.
- **The CI runner for the aarch64 tier** is decided at stage 8, after
  measuring whether hosted `ubuntu-24.04-arm` exposes `/dev/kvm` and whether
  HVF is usable inside a hosted `macos-latest` runner's VM.
- **Hardware: none is bought now.** The eventual target must be small and
  laptop-class, like the Lenovo T14 the project already drives. See "Hardware
  evidence" below; neither researched machine is a target.

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

**Hardware ARM lacks.** `toyos-i219` (9,250 lines) is the T14's NIC: portable
code for hardware no ARM board in scope has.

**Inline assembly**, 834 lines by a paren-depth scan of
`asm!`/`naked_asm!`/`global_asm!` bodies (undercounts: macro bodies like
`switch_save!` are not counted). 626 lines are in `arch/`. Outside it:
`loader/start.rs` 47, `sched/driver.rs` 20 (plus the `switch_*` macros at
`sched/driver.rs:1019-1062`), `irq_census.rs` 19, `nmi_gate.rs` 16, `main.rs`
13, `drivers/serial.rs` 13, `hw.rs` 10, `blackbox.rs` 6, `iommu/vtd/table.rs`
6, `panic_console/mod.rs:1294` 1, `xhci/wait/mod.rs:36` 1. Userland plus
`toyos-abi`: 54 lines (`libc/memory.rs` 23, `toyos-abi/syscall.rs` 20). The
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
  `boot_pml4_addr`, no DTB field — consistent with ACPI-only.
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
`toyos-cc` (10,576 lines) emits only x86.

**Already abstracted.** The syscall stub already has both arms
(`toyos-abi/src/syscall.rs:678,703`: `syscall` and `svc #0`). `toyos-sched`
(8,099 lines) is pure behind `Machine`/`Hw`
(`toyos-sched/src/hw.rs:88-158`: `now`, `set_timer`, `stop_timer`,
`irq_guard`, `halt`, `need_resched`, `switch`), with `kernel/src/hw.rs` as
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
(variant I and TLSDESC across `toyos-ld`, the kernel loader and std;
`toyos-cc` has no AArch64 backend); the test harness (33,907 lines in
`tests/common/`, written against q35, i8042, OVMF, KVM, `intel-iommu`).

## Hardware evidence

Checked against sources on 2026-09-26. Neither machine is a target now.

**Radxa Orion O6N (CIX P1 CD8160, checked 2026-09-26).** SystemReady SR v2.5
is certified for the sibling **O6**, not the O6N — Radxa states only "SBSA
Level 6" for the O6N, and no O6N certificate was found. EDK2 UEFI firmware
offers ACPI or DT. Mainline's `sky1.dtsi` declares `arm,gic-v3` with
`arm,gic-v3-its`, `arm,armv8-timer`, `arm,psci-1.0` (method `smc`) — SR's BSA
requires all three. **No SMMU is named in mainline `sky1.dtsi`, and no source
confirms one exists** — unverified, and ToyOS's userland-driver model needs
one; read it from firmware IORT before relying on it. CPUs are 12-core and
**heterogeneous**: 4× Cortex-A720 performance, 4× A720 balanced, 4×
Cortex-A520 efficiency — a live case for
`issues/kernel/all-cores-are-assumed-equal-and-arm64-breaks-that.md`. Serial
is a real PL011 (UART2 on the 40-pin header, 115200 8n1). PCIe is CIX's own
host bridge (5 controllers); an M.2 M-key NVMe slot is Gen4 x4; the xHCI IP
is unverified. Idle power on the O6 measures 15.8-16.6 W; no O6N figure was
found.

**Lenovo ThinkPad T14s Gen 6 (Snapdragon X1E-78-100, checked 2026-09-26).**
The firmware boots the OS at **EL1 under Gunyah/QHEE**; EL2 is reachable only
through `slbounce`, which runs Microsoft's signed `tcblaunch.exe` on the CPU
(ruled out above). Qualcomm's ACPI targets Windows PEP power plugins; Linux
boots this platform with DT, installed by `DtbLoader.efi`. `x1-el2.dtso`
states plainly that under Gunyah "this IOMMU is controlled by the firmware"
and "ITS emulation in Gunyah is broken so we can't use MSI on some PCIe
controllers in EL1" — at EL1 the OS does not own the PCIe SMMU, so this
laptop cannot host userland drivers even if DT were in scope. No community
source documents an exposed debug UART on this board — treat as none. GOP
works only at a fixed 1360×855, with no native-resolution mode even from the
EFI shell.

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
  language on SystemReady hardware.
- **Pure crates stay arch-free.** A per-arch decision that becomes pure lives
  in its own crate, as `toyos-bootmap` does: it grows a TTBR plan, and
  `toyos-pcid` becomes an ASID/PCID allocator.

## Stages

Each stage names its exit; "measured" means a number from a run.

0. **Rulings and shared seams, before any aarch64 file exists.** `src/` gains
   an `Arch`, replacing the 57 triple mentions. `arch/syscall/` minus
   `gate.rs` moves out of `arch/`. `toyos-elf` and `toyos-ld` take a machine:
   `EM_AARCH64`, `R_AARCH64_RELATIVE`, PE `0xAA64`. The MSI doorbell becomes
   one arch-provided constant. `IrqGuard`/`LogCommitGuard` become one arch
   primitive. The loom model owed by
   `issues/kernel/the-stops-no-lost-wake-claim-rests-on-x86-locked-rmws.md`
   lands, and the `Mmio` barrier contract above is written and asserted.
   **Exit**: x86 builds and passes unchanged. `rg 'x86_64-unknown' src/`
   names one `Arch` table. `toyos-elf` and `toyos-ld` host tests cover an
   aarch64 PIE with relocations.

1. **`aarch64-unknown-toyos` in the rust fork and `toyos-ld`.** A target
   spec: `aarch64-unknown-none-elf`, PIC, frame pointers, `toyos-ld` as the
   linker. Std pal `_start` and TLS for variant I with TLSDESC. `toyos-ld`
   handles the TLSDESC/TLSLE relocation families and `aarch64` stubs in ELF
   output. **Exit**: `cargo +toyos build --target aarch64-unknown-toyos`
   builds `std` plus a hello-world and the whole Rust userland. `toyos-ld`'s
   output passes `toyos-elf` and `llvm-readobj` with no unknown relocations.

2. **The UEFI loader on AArch64** (`aarch64-unknown-uefi`, tier 2 upstream,
   no fork work needed). Entry is `extern "C"` rather than `sysv64`.
   `toyos-bootmap` grows a TTBR0/TTBR1 plan. The loader's TSC/CPUID/MSR and
   TCO lines become per-arch. `KernelArgs` carries what the arch needs (RSDP
   remains; no DTB field, per the ACPI-only ruling). **Exit**: under
   `qemu-system-aarch64 -M virt` with edk2, the loader reads ROOT through
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

5. **SMP through PSCI.** `CPU_ON` from MADT GICC entries, SGIs as the IPI,
   broadcast TLBI behind the machine-wide invalidation contract. **Exit**:
   `-smp 8` boots all CPUs; every CPU asserts the control-register
   declaration; the shootdown stress test and the loom-checked stop both
   pass; `CPU_OFF`/`SYSTEM_RESET`/`SYSTEM_OFF` replace ACPI reset and PM1a.

6. **Virtio on `virt`.** virtio-pci (ECAM from MCFG) for blk, net, gpu,
   sound, input and rng. virtio-input replaces the i8042 as the
   key-transition source. virtio-rng, or SMCCC TRNG, feeds `sys_random`
   alongside RNDR. SMMUv3 is on `virt` (`-M virt,iommu=smmuv3`), decoded from
   IORT. **Exit**: netd claims its NIC through an SMMUv3 domain; a
   foreign-DMA test faults into a `DMA FAULT` record, not a crash.

7. **Userland boots.** `init`, `logd`, the compositor, netd, soundd and sshd,
   built for `aarch64-unknown-toyos`. C programs stay x86-only per the
   `toyos-cc` ruling. **Exit**: the desktop comes up on virtio-gpu; `ssh`
   works from the host; `/log` survives a reboot; the same `system.toml`
   drives both arches.

8. **An aarch64 tier in the harness.** `tests/common/qemu.rs` takes an
   `Arch`: `virt`, edk2-aarch64, HVF on Apple hosts (TCG otherwise).
   `src/tiers.rs` gains the arch axis. `src/redlist.rs` quarantines per arch.
   The CI runner is picked here, after measuring hosted `macos-latest`
   (whether HVF is usable inside the runner VM) and hosted
   `ubuntu-24.04-arm` (whether `/dev/kvm` exists there). **Exit**: the fast
   tier runs on aarch64 locally on the M4 host; the nightly runs the aarch64
   tier on whatever runner that measurement picks; a test red on only one
   arch is a named known-red, not a skip.

9. **The SystemReady board (Orion O6N), if its SMMU confirms.** The same
   image boots from USB on the board's own UEFI+ACPI with no board-specific
   code: SPCR selects the UART, MADT/GTDT/IORT/PPTT describe the rest. NVMe
   and xHCI drivers on real PCIe. An RTL8126/8125 driver in netd, or a USB
   NIC, for the answer path. A per-core capacity model from PPTT/`MIDR` for
   the A720/A520 split
   (`issues/kernel/all-cores-are-assumed-equal-and-arm64-breaks-that.md`).
   The metal loop gains a serial channel on the header's UART2. **Exit**: an
   unattended boot over the metal loop reports through serial and the log
   partition; NVMe root, USB keyboard and network all work; the black box's
   "DRAM survives reset" invariant is measured on this board, not assumed.

10. **The laptop — rejected for now.** The owner ruling above declines
    `tcblaunch.exe`; at EL1 a Snapdragon laptop does not give ToyOS its PCIe
    SMMU (`x1-el2.dtso`, quoted above), so it cannot host userland drivers.
    No Snapdragon laptop is a target. Revisit only if the owner reverses that
    ruling.

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
- **The black box and the T14 bench** (`toyos-blackbox`, `src/metal.rs`):
  the black box's DRAM-survives-reset assumption is re-measured per ARM
  machine, not carried over. The loop's watchdog is Intel TCO; ARM
  SystemReady offers the SBSA generic watchdog through GTDT.
  `toyos-metal`'s flash-over-ssh design transfers to the O6N unchanged and
  gains a serial channel.
- **Heterogeneous cores**
  (`issues/kernel/all-cores-are-assumed-equal-and-arm64-breaks-that.md`): the
  O6N makes it real at stage 9.
- **Page size**
  (`issues/kernel/process-memory-is-2-mib-pages-and-that-caps-the-process-count.md`):
  2 MiB L2 blocks map identically on both arches, so no change is forced by
  this track.
