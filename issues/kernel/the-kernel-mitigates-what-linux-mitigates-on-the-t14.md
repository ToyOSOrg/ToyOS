---
status: open
kind: track
opened: 2026-09-29
---

# The kernel mitigates what Linux mitigates on the T14

Owner ruling: ToyOS also needs to be at least as secure as Linux on the same
hardware. The hardware is the T14 (`issues/hardware/the-t14-boots-toyos-unattended.md`),
an i5-1135G7, Tiger Lake, family 6 model 0x8C
(`issues/hardware/the-t14-touchpad-is-i2c-hid-and-unbuilt.md`). The Linux is
torvalds/linux at v6.16, the tag every Linux citation here pins. Its
`/sys/devices/system/cpu/vulnerabilities/*` lines on the T14 are the
scoreboard, with the defaults those lines do not show: KASLR
(`arch/x86/Kconfig` RANDOMIZE_BASE `default y`), kernel IBT (X86_KERNEL_IBT
`def_bool y`), user-space ASLR and the stack protector.

## What is true today

The kernel writes no CPU-vulnerability mitigation: no access to
`IA32_SPEC_CTRL` (0x48), `IA32_PRED_CMD` (0x49), `IA32_ARCH_CAPABILITIES`
(0x10A) or `IA32_FLUSH_CMD` (0x10B) anywhere in `kernel/src/`, no `lfence`
speculation barrier, no `verw` buffer clear, no retpoline or return-thunk
codegen flag. It executes no `swapgs`: GS holds kernel per-CPU data
permanently (`kernel/src/arch/x86_64/syscall.rs`, above `syscall_entry`) and
`FSGSBASE` is `CR4_FORBIDDEN` (`kernel/src/arch/x86_64/control_regs.rs`).
SMEP, SMAP and UMIP are taken only when offered (`CR4_OPTIONAL`, same file).
Every user process loads at `USER_VM_BASE` (`kernel/src/loader/mod.rs`) with
its stack directly under the image (`kernel/src/vma.rs`, `ALLOC_CEILING =
STACK_BASE`) and its mmap window at fixed bounds (`vma.rs`, `WINDOW`), and
`rebase_base` (`toyos-userbound/src/span.rs`) takes no random input. The
direct map sits at the fixed `PHYS_OFFSET` (`kernel/src/mm/mod.rs`) and the
kernel image at `PHYS_OFFSET` plus wherever firmware allocated it
(`bootloader/src/main.rs`): nothing draws either. The kernel target is
`x86_64-unknown-none` with no stack-protector or `cf-protection` flag
(`kernel/.cargo/config.toml`).

## Stages

System-mode TCG enumerates none of SPEC_CTRL, ARCH_CAPABILITIES and SSBD
(`target/i386/cpu.c` at QEMU v11.1.1: `CPUID_7_0_EDX_KERNEL_FEATURES` is 0
outside `CONFIG_USER_ONLY`), nor IBT or BHI_CTRL (`TCG_7_0_EDX_FEATURES`,
`TCG_7_2_EDX_FEATURES`), and `-cpu host` needs KVM or HVF on an x86 host.
Every branch gated on those bits, each MSR write below included, is
exercised on the T14 alone; QEMU exercises the absent branches and the
software sequences S1 selects for the harness's TCG `-cpu` string
(`src/arch.rs`, `Arch::cpu`).

- **S0 — Evidence. T14-only.** Under Linux v6.16 on the T14: every
  vulnerabilities file, `/proc/sys/vm/mmap_rnd_bits`, CPUID leaves 1, (7,0)
  and (7,2), the microcode revision read as Linux reads it (write 0 to 0x8B,
  CPUID(1), read 0x8B: `arch/x86/include/asm/microcode.h`
  `intel_get_microcode_revision`), 0x10A, `IA32_MCU_OPT_CTRL` (0x123) and,
  where ARCH_CAPABILITIES bit 7 (TSX_CTRL_MSR) is set, `IA32_TSX_CTRL`
  (0x122). Linux v6.16 in a TCG guest on the T14 under the harness's TCG
  `-cpu` string: every vulnerabilities file. ToyOS on the T14: the same CPUID
  leaves and 0x10A, and 0x48 and 0x123 as read before the kernel's first
  write to either, the reset values Linux has already overwritten. **Exit**:
  those facts and strings committed as S1's fixtures.
- **S1 — The decision, a host-tested function.** A pure function, in a crate
  the kernel and a host test both build, maps (vendor, family, model,
  stepping, microcode revision, CPUID.1, CPUID.(7,0), CPUID.(7,2),
  ARCH_CAPABILITIES) to each vulnerability's Linux line and the mitigation
  ToyOS applies. It carries v6.16's `cpu_vuln_whitelist` and
  `cpu_vuln_blacklist` (`arch/x86/kernel/cpu/common.c`),
  `microcode/intel-ucode-defs.h`, and `cpu_set_bug_bits` and `bugs.c`'s
  selections under the default command line. CPUID.(7,0):EDX bit 26 is
  SPEC_CTRL (IBRS and IBPB), 27 STIBP, 29 ARCH_CAPABILITIES and 31 SSBD
  (`arch/x86/include/asm/cpufeatures.h`, word 18); the kernel reads 0x10A
  only when bit 29 is set, as `x86_read_arch_cap_msr` does, since the read is
  `#GP` otherwise. **Exit**: host tests feed S0's T14 facts and get S0's T14
  lines, and feed the TCG model's facts and get the TCG guest's lines. A
  clause naming a VM-exit mitigation (`PBRSB-eIBRS: SW sequence`, `KVM: SW
  loop`) is classified "no counterpart": ToyOS hosts no guest. Negative
  control: deleting the TIGERLAKE_L row from either table reds the T14 case.
  The QEMU boot prints the TCG model's lines from the kernel's own CPUID
  reads.
- **S2 — SPEC_CTRL and GDS. T14-only.** The per-CPU base of `IA32_SPEC_CTRL`
  is declared once with the control registers and asserted by `self_check`
  on every CPU: IBRS where ARCH_CAPABILITIES.IBRS_ALL (eIBRS), BHI_DIS_S where
  CPUID.(7,2):EDX[4] (BHI_CTRL). S6's SSBD is the one bit a context switch
  changes, so the assertion is `SPEC_CTRL & !SSBD == base`. Where S1 finds
  GDS and ARCH_CAPABILITIES.GDS_CTRL, `IA32_MCU_OPT_CTRL.GDS_MITG_DIS` is
  cleared and read back and `GDS_MITG_LOCKED` is never written, as
  `update_gds_msr` does; a lock firmware set is reported as Linux reports it,
  "Mitigation: Microcode (locked)". **Exit**: on every T14 CPU the
  read-backs match the declaration, and the `spectre_v2` eIBRS clause and
  `gather_data_sampling` equal S0's.
- **S3 — The boundaries.** The branch history is cleared in software on
  syscall entry where S1 finds BHI and no BHI_CTRL, and S2's BHI_DIS_S
  replaces the loop where BHI_CTRL is present (`bugs.c`
  `bhi_apply_mitigation`); Tiger Lake has eIBRS without BHI_CTRL, so the T14
  takes the loop. The RSB is filled on context switch where S1 selects a mode
  Linux fills in, retpoline, LFENCE or IBRS and never eIBRS
  (`spectre_v2_select_rsb_mitigation`). IBPB is conditional (Decisions).
  **Exit**: the `syscall_entry` byte gate (`src/build.rs`) reds on an entry
  without the clear sequence. QEMU, whose model S1 puts in retpoline mode
  (no eIBRS: `spectre_v2_select_mitigation`), counts one RSB fill per switch
  under `boot-actuators`. The IBPB decision is a pure function of the
  per-CPU last-user state and the incoming process, host-tested on A→idle→B
  (IBPB when either is flagged) and A→idle→A (none); deciding from the
  previous root instead, which on A→idle→B is idle's kernel root
  (`kernel/src/arch/x86_64/hw.rs` activates one on every switch), reds it.
  The T14 counts the `IA32_PRED_CMD` writes under `boot-actuators`.
- **S4 — Spectre v1.** SMAP moves to the required set: the kernel refuses to
  boot on an x86-64 CPU without it. With no `swapgs` executed and `FSGSBASE`
  forbidden, Linux's own rule (`spectre_v1_apply_mitigation`: `FSGSBASE ||
  !smap_works_speculatively()`) then owes no swapgs barrier. The usercopy
  barrier and user-pointer sanitization go where the range is checked: an
  `lfence` after `in_user_half` in `kernel/src/user_ptr.rs`'s `window` and
  after `is_user_object` in its `object`, and the checked address masked to
  the user half before `translate_user` walks it, as `barrier_nospec` in
  `_inline_copy_from_user` (`include/linux/uaccess.h`) and
  `mask_user_address` (`arch/x86/include/asm/uaccess_64.h`) do. **Exit**:
  the TCG model without `+smap` refuses to boot naming SMAP, run once as a
  mutation; the T14 times a bounds-check-bypass gadget through `window`
  with and without the barrier.
- **S5 — Indirect branches and returns.** Every indirect `call`/`jmp` goes
  through `-Zretpoline-external-thunk` and every return through
  `-Zfunction-return=thunk-extern` into the kernel's own thunks, whose body
  is the one S1 selects: a retpoline, or for ITS an aligned thunk. ITS
  predicts indirect branches and RETs whose last byte lies in a cacheline's
  lower half, and Linux's thunks put that byte in the upper half
  (`Documentation/admin-guide/hw-vuln/indirect-target-selection.rst`,
  "Mitigation"). The retpoline flag is a target modifier, so the
  `x86_64-unknown-none` `core` and `alloc` that `src/toolchain.rs` builds
  are built under it. **Exit**: a gate over `kernel.elf` finds no raw
  indirect `call`/`jmp` or `ret` outside the thunks and the entry sequences
  it names by symbol, and asserts every thunk branch's last byte has
  `addr & 63 >= 32`; placing a thunk's branch at a cacheline start reds it.
- **S6 — Speculative Store Bypass.** `IA32_SPEC_CTRL.SSBD` is set while a
  flagged process runs and clear otherwise, Linux's default ("Mitigation:
  Speculative Store Bypass disabled via prctl", `bugs.c`
  `ssb_select_mitigation`). The flag is one bit in `SpawnArgs` on the
  existing `SYS_SPAWN` (`toyos-abi/src/syscall.rs`), set by init from the
  program's `system.toml` entry and inherited by whatever a flagged process
  spawns; it also marks the process for S3's IBPB. **Exit**: T14-only, a
  guest test reads the bit back clear in an unflagged process and set in a
  flagged one.
- **S7 — Microcode loading. PARKED for the owner.** Gather Data Sampling and
  some ITS variants are microcode-only fixes; Linux's coverage of them on the
  T14 assumes the shipped BIOS carries microcode at or past the fixing
  revision. Root `CLAUDE.md`'s dependency rule allows vendor firmware only
  "loaded only by that device's own driver through its IOMMU domain; it never
  executes on the CPU" — a CPU microcode update is firmware that *does*
  execute on the CPU, the one case that rule does not carve out. This stage is
  needed only if S0 finds the T14's shipped BIOS revision is below the
  revision Linux's mitigation table requires; if S0 finds the BIOS already at
  or past it, this stage is moot and is closed without being built. Either way
  it is the owner's call, not this track's.
- **S8 — User-space ASLR.** The image base, the stack base and the mmap
  window's base are each drawn per spawn from `arch::entropy::draw`. Each
  base's entropy is stated in bits against Linux's: S0's `mmap_rnd_bits` for
  the image and mmap bases, 22 bits for the stack
  (`arch/x86/include/asm/elf.h` `STACK_RND_MASK` 0x3fffff pages). At 2 MiB
  alignment the 2^47-byte user half holds 2^26 slots, so a base short of
  Linux's figure is filed as a defect with both numbers. **Exit**: a guest
  test spawns one binary 256 times, each run reporting its three bases from
  its own addresses; every claimed bit of every base's slot index is set in
  between 88 and 168 runs (±5σ; per-bit false-red probability 3.3e-7, exact
  binomial tail). `base = BASE + n * 2 MiB` leaves every slot bit above bit 7
  clear in all 256 runs and reds it.
- **S9 — Kernel stack protector.** `-Zstack-protector=strong` for the kernel
  and its `core` and `alloc` (`src/toolchain.rs`). On `x86_64-unknown-none`
  LLVM reads a global `__stack_chk_guard`, which the kernel defines and seeds
  from `arch::entropy::draw` in a frame that never returns, before any
  protected frame is entered. One global canary is a gap versus Linux's
  per-task one (`arch/x86/include/asm/stackprotector.h`), open after this
  stage. **Exit**: the guest test `kernel_stack_canary` overflows a
  `boot-actuators` frame with zeros and asserts the panic names the stack
  protector; `static __stack_chk_guard: u64 = 0` passes the check, returns
  through a zeroed address and reds it.
- **S10 — Kernel ASLR.** The kernel image's virtual base and the direct map's
  base are drawn per boot before the kernel runs, each with its bits stated
  against Linux's: RANDOMIZE_BASE puts the text in a 2 MiB slot of
  `KERNEL_IMAGE_SIZE` = 1 GiB (`arch/x86/include/asm/page_64_types.h`, at
  most 9 bits), and RANDOMIZE_MEMORY draws `page_offset_base` at PUD
  granularity (`arch/x86/mm/kaslr.c`). **Exit**: 64 QEMU boots each print
  both bases on a `boot-actuators` line; every claimed bit is set in between
  12 and 52 of them (per-bit false-red probability 1.0e-7); a fixed base
  reds it.
- **S11 — Kernel IBT. T14-only.** `-Zcf-protection=branch` for the kernel
  and its `core` and `alloc`, and `CR4.CET` with `IA32_S_CET` (0x6A2)
  ENDBR_EN declared with the control registers where CPUID.(7,0):EDX[20]
  (IBT) is set. **Exit**: a `boot-actuators` indirect call past a function's
  `endbr64` panics naming `#CP`; with `CR4.CET` left clear it returns and
  reds.

## Decisions

- **IBPB is conditional**, Linux's default: `spectre_v2_user_select_mitigation`
  maps the default command to PRCTL, which enables `switch_mm_cond_ibpb`, and
  `arch/x86/mm/tlb.c` `cond_mitigation` issues IBPB on a switch between two
  processes only when either is flagged, printing "IBPB: conditional". The
  flag is S6's.
- **Probes add no syscall.** They are compiled under `boot-actuators` or
  checked by the `syscall_entry` byte gate; the one ABI change is S6's bit.

## Exit

On the T14, ToyOS prints for every Linux vulnerabilities file the line S1
computes, S1's host tests hold each equal to S0's Linux line, S2–S6 and
S8–S11 are green, and S7 is closed by S0's finding or by the owner.
