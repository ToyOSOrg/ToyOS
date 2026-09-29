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
the kernel the T14 runs, Ubuntu's 6.8.0-142-generic. Its source is tag
`Ubuntu-6.8.0-142.142` (commit 53e5d07aac028a1523ab0b115f079d6d1bc831ef) of
`https://git.launchpad.net/~ubuntu-kernel/ubuntu/+source/linux/+git/noble`,
where every Linux `path:line` here is read; `bugs.c` and `common.c` are in
`arch/x86/kernel/cpu/`. Its config is `/boot/config-6.8.0-142-generic` from
`linux-modules-6.8.0-142-generic_6.8.0-142.142_amd64.deb`, sha256
3b8533dd9d235ca634ac58f82c5ce1ee35f12ef620693e17033184d2c9ca5890. The
scoreboard is that kernel's `/sys/devices/system/cpu/vulnerabilities/*` lines
on the T14, and the config's defaults no line shows: `CONFIG_RANDOMIZE_BASE`,
`CONFIG_RANDOMIZE_MEMORY`, `CONFIG_STACKPROTECTOR_STRONG`,
`CONFIG_ARCH_MMAP_RND_BITS=32`, and `CONFIG_SLS`, the one entry of the
`CPU_MITIGATIONS` menu (`arch/x86/Kconfig:2475-2664`) that no line reports.
The config leaves `CONFIG_X86_KERNEL_IBT` unset.

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
outside `CONFIG_USER_ONLY`), nor BHI_CTRL (`TCG_7_2_EDX_FEATURES`). The
nightly's `guest` and `audio` shards run KVM with `-cpu host`
(`.github/workflows/nightly.yml`, `src/arch.rs` `Arch::cpu`), so there the
branches gated on those bits run on whatever CPU the runner has. A QEMU test
therefore asserts wiring only: what the guest applied equals S1 applied to
the facts the guest itself reports on a `boot-actuators` line. Independence
comes from S1's T14 fixture and the T14 run, never from KVM.

- **S0 — Evidence.** On the T14 under its stock Ubuntu boot, as root:

  ```
  apt-get install -y msr-tools cpuid busybox-static cpio && modprobe msr
  cat /proc/version /proc/cmdline /proc/sys/vm/mmap_rnd_bits
  dpkg-query -W linux-image-6.8.0-142-generic linux-modules-6.8.0-142-generic
  sha256sum /boot/config-6.8.0-142-generic
  grep . /sys/devices/system/cpu/vulnerabilities/*
  grep microcode /proc/cpuinfo
  cpuid -r -l 1; cpuid -r -l 7 -s 0; cpuid -r -l 7 -s 2
  rdmsr -a 0x10a; rdmsr -a 0x48; rdmsr -a 0x123
  grep x86_Thread_features /proc/self/status
  mkdir -p rd/bin rd/sys rd/proc && cp /usr/bin/busybox rd/bin/
  printf '#!/bin/busybox sh\n/bin/busybox mount -t sysfs s /sys\n/bin/busybox mount -t proc p /proc\n/bin/busybox cat /proc/version\n/bin/busybox grep . /sys/devices/system/cpu/vulnerabilities/*\n/bin/busybox poweroff -f\n' > rd/init && chmod +x rd/init
  (cd rd && find . | cpio -o -H newc) > s0.cpio && cp /boot/vmlinuz-6.8.0-142-generic .
  ```

  and `rdmsr -a 0x122` where 0x10A bit 7 (TSX_CTRL_MSR) is set. `/proc/cpuinfo`'s
  `microcode` is the revision as Linux reads it
  (`arch/x86/include/asm/microcode.h:62-75`). On the development host, whose
  QEMU is the one `.github/qemu-version` pins, `qemu-system-x86_64 --version`
  and then `qemu-system-x86_64 -nodefaults -machine q35 -cpu
  qemu64,+rdrand,+smap,+fsgsbase,+x2apic,+smep -smp 2 -m 4G -kernel
  vmlinuz-6.8.0-142-generic -initrd s0.cpio -append "console=ttyS0 panic=-1"
  -serial stdio -display none -no-reboot` capture the TCG model's lines. ToyOS,
  on the T14 and on the TCG model, prints a `boot-actuators` line with the
  same CPUID leaves and 0x10A, and 0x48 and 0x123 as read before the kernel's
  first write to either. The capture is refused unless `/proc/version` names
  6.8.0-142, both packages are 6.8.0-142.142 and the config's sha256 is the one
  above. If `x86_Thread_features` shows `shstk`, user shadow stacks are owed
  and filed as their own track. **Exit**: those outputs committed as S1's
  fixtures. Ubuntu is wiped from the T14 only after that commit.
- **S1 — The decision, a host-tested function.** A pure function, in a crate
  the kernel and a host test both build, maps (vendor, family, model,
  stepping, microcode revision, CPUID.1, CPUID.(7,0), CPUID.(7,2),
  ARCH_CAPABILITIES) to each vulnerability's Linux line and the mitigation
  ToyOS applies. It carries `cpu_vuln_whitelist` and `cpu_vuln_blacklist`
  (`common.c:1182-1344`), `cpu_set_bug_bits` (`common.c:1414-1578`),
  `spectre_bad_microcodes` (`arch/x86/kernel/cpu/intel.c:141-163`) and
  `bugs.c`'s selections under the default command line and the pinned config.
  CPUID.(7,0):EDX bit 26 is SPEC_CTRL (IBRS and IBPB), 27 STIBP, 29
  ARCH_CAPABILITIES and 31 SSBD (`arch/x86/include/asm/cpufeatures.h:434-439`),
  and CPUID.(7,2):EDX bit 4 is BHI_CTRL (`arch/x86/kernel/cpu/scattered.c:31`);
  the kernel reads 0x10A only when bit 29 is set, as `x86_read_arch_cap_msr`
  does (`common.c:1353-1361`), since the read is `#GP` otherwise. **Exit**:
  host tests feed S0's T14 facts and get S0's T14 lines, and feed the TCG
  model's facts and get the TCG model's lines. Negative control: deleting
  `GDS` from the TIGERLAKE_L blacklist row (`common.c:1312`) reds the T14
  case. That match is the only way `X86_BUG_GDS` is set (`common.c:1521-1523`),
  and without the bug `cpu_show_common` answers "Not affected"
  (`bugs.c:3308-3309`), where the T14 read "Mitigation: Microcode" (59c096b9 on
  `wt/toyos-llvmbar`).
- **S2 — SPEC_CTRL and GDS.** The per-CPU base of `IA32_SPEC_CTRL` is declared
  once with the control registers and asserted by `self_check` on every CPU:
  IBRS where ARCH_CAPABILITIES.IBRS_ALL (eIBRS, `bugs.c:1930-1937`), BHI_DIS_S
  where BHI_CTRL (`bugs.c:1795-1805`). S6's SSBD is the one bit a context switch
  changes, so the assertion is `SPEC_CTRL & !SSBD == base`. Where S1 finds GDS
  and ARCH_CAPABILITIES.GDS_CTRL, `IA32_MCU_OPT_CTRL.GDS_MITG_DIS` is cleared
  and read back and `GDS_MITG_LOCKED` is never written, as `update_gds_msr`
  does (`bugs.c:777-812`); a lock firmware set is reported as Linux reports it,
  "Mitigation: Microcode (locked)" (`bugs.c:850-861`). **Exit**, T14: on every
  CPU 0x48 reads the declared base, which equals S0's Linux read of 0x48 with
  SSBD masked; a `boot-actuators` arm sets `GDS_MITG_DIS` on every CPU before
  the kernel's GDS write and reads it clear after, so deleting the clear
  leaves it set and reds it. If S0 finds `GDS_MITG_LOCKED` set, the arm cannot
  set it and S0's fixture says so. On KVM the read-back equals S1's base over
  the reported facts.
- **S3 — The boundaries.** The branch history is cleared in software on
  syscall entry where S1 finds BHI and no BHI_CTRL (`bugs.c:1831-1858`,
  `arch/x86/entry/entry_64.S:119`), and S2's BHI_DIS_S replaces the loop where
  BHI_CTRL is present; Tiger Lake has eIBRS without BHI_CTRL, so the T14 takes
  the loop. The RSB is filled on context switch where S1 selects retpoline,
  LFENCE or IBRS mode and never under eIBRS (`bugs.c:1741-1789`,
  `entry_64.S:205`). IBPB is conditional (Decisions). **Exit**: a
  `boot-actuators` per-thread counter the clear sequence increments advances
  by exactly 1000 over one thread's 1000 syscalls where S1 selects the loop,
  the T14, and by 0 where it does not; inverting the runtime condition gives 0
  on the T14 and reds it. A `boot-actuators` count of RSB fills over a probe's
  context switches equals the switch count where S1 over the reported facts
  selects a filling mode and 0 otherwise; the TCG model is in retpoline mode
  (`common.c:1231` lacks NO_SPECTRE_V2, and AUTO without eIBRS or RETBLEED
  takes retpoline, `bugs.c:1878-1895`), so dropping the fill reds it there.
  The IBPB decision is a pure function of the per-CPU last-user state and the
  incoming process, host-tested on A→idle→B (IBPB when either is flagged) and
  A→idle→A (none); deciding from the previous root instead, which on A→idle→B
  is idle's kernel root (`kernel/src/arch/x86_64/hw.rs` activates one on every
  switch), reds it. A `boot-actuators` arm drives B, C, idle, B, A, idle, A, B
  through the switch path on one CPU with only A flagged and counts
  `IA32_PRED_CMD` writes: 2 (B→A, A→B) where S1 finds IBPB, the T14, and 0
  where it does not; dropping the write from the switch path gives 0 on the
  T14 and reds it.
- **S4 — Spectre v1.** SMAP moves to the required set: the kernel refuses to
  boot on an x86-64 CPU without it. With no `swapgs` executed and `FSGSBASE`
  forbidden, Linux's own rule (`bugs.c:943-944`: `FSGSBASE ||
  !smap_works_speculatively()`) then owes no swapgs barrier. The usercopy
  barrier and user-pointer sanitization go where the range is checked: an
  `lfence` after `in_user_half` in `kernel/src/user_ptr.rs`'s `window` and
  after `is_user_object` in its `object`, and the checked address masked to
  the user half before `translate_user` walks it, as `barrier_nospec` in
  `_copy_from_user` (`lib/usercopy.c:21`) and `mask_user_address`
  (`arch/x86/include/asm/uaccess_64.h:64`) do. **Exit**: a standing guest test
  boots the TCG model without `+smap` and asserts the refusal names SMAP;
  moving SMAP back to `CR4_OPTIONAL` boots and reds it. On the T14 a
  bounds-check-bypass gadget through each of the three sites recovers a byte
  planted outside the user half in at most 16 of 1000 trials (chance is 1 in
  256; per-site false-red probability 7.6e-7, exact binomial tail). Each
  site's mutation, its `lfence` or the mask deleted, must recover it in more
  than 16, or that site is unmeasured and the stage is not done.
- **S5 — Indirect branches and returns.** Every indirect `call`/`jmp` goes
  through `-Zretpoline-external-thunk` and every return through
  `-Zfunction-return=thunk-extern` into the kernel's own thunks, whose body
  is the one S1 selects: a retpoline, or for ITS an aligned thunk whose
  branch's last byte lies in a cacheline's upper half
  (`Documentation/admin-guide/hw-vuln/indirect-target-selection.rst:6-8,56-59`,
  `arch/x86/lib/retpoline.S:371-403`), which the T14 takes
  (`bugs.c:1222-1223,1313-1319`). The retpoline flag is a target modifier, so
  the `x86_64-unknown-none` `core` and `alloc` that `src/toolchain.rs` builds
  are built under it. Every `ret` and indirect `jmp` is followed by `int3`,
  as `CONFIG_SLS` builds Linux (`-mharden-sls=all`, `arch/x86/Makefile:210`);
  the rustc fork has no option for it (`compiler/rustc_session/src/options.rs`).
  **Exit**: a gate over `kernel.elf` finds no raw indirect `call`/`jmp` or
  `ret` outside the thunks and the entry sequences it names by symbol, an
  `int3` after every `ret` and indirect `jmp`, and every aligned thunk's
  branch ending at `addr & 63 >= 32`; placing a thunk's branch at a cacheline
  start, or building without the SLS hardening, reds it. Each boot reads its
  live thunk bodies back and compares them with the body S1 selects over the
  facts it reports: a `thunk_body` that answers the aligned thunk for every
  input puts a bare `jmp *%reg` in the TCG model, whose selection is the
  retpoline, and reds it.
- **S6 — Speculative Store Bypass.** `IA32_SPEC_CTRL.SSBD` is set while a
  flagged process runs and clear otherwise, Linux's default (`bugs.c:2200-2202`:
  the default command selects `SPEC_STORE_BYPASS_PRCTL`). The flag is one bit
  in `SpawnArgs` on the existing `SYS_SPAWN` (`toyos-abi/src/syscall.rs`), set
  by init from the program's `system.toml` entry and inherited by whatever a
  flagged process spawns; it also marks the process for S3's IBPB. **Exit**,
  T14: a `boot-actuators` probe logs `IA32_SPEC_CTRL.SSBD` as read on the first
  switch into each process, and a guest test reads it clear for an unflagged
  process, set for a flagged one, and set for a child a flagged process spawns
  with the bit clear in its `SpawnArgs`; dropping the inheritance at spawn
  clears the child's and reds it. On KVM the expected bits are S1's over the
  reported facts.
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
  window's base are each drawn per spawn from `arch::entropy::draw`; a draw
  still `None` after `entropy::ATTEMPTS` refuses the spawn. Each base's entropy
  is stated in bits against Linux's: `CONFIG_ARCH_MMAP_RND_BITS=32` for the
  image and mmap bases (`fs/binfmt_elf.c:1127-1129`, `arch/x86/mm/mmap.c:77`),
  22 bits for the stack (`arch/x86/include/asm/elf.h:331`, `STACK_RND_MASK`
  0x3fffff pages). At 2 MiB alignment the 2^47-byte user half holds 2^26
  slots, so a base short of Linux's figure is filed as a defect with both
  numbers. **Exit**: a guest test spawns one binary 256 times, each run
  reporting its three bases from its own addresses; every claimed bit of every
  base's slot index is set in between 88 and 168 runs (±5σ; per-bit false-red
  probability 3.3e-7, exact binomial tail). `base = BASE + n * 2 MiB` leaves
  every slot bit above bit 7 clear in all 256 runs and reds it. Each of S10's
  64 boots also prints its first spawn's three bases, bit-tested by S10's
  rule; a constant-seeded generator repeats them every boot and reds it.
- **S9 — Kernel stack protector.** `-Zstack-protector=strong` for the kernel
  and its `core` and `alloc` (`src/toolchain.rs`). On `x86_64-unknown-none`
  LLVM reads a global `__stack_chk_guard`, which the kernel defines and seeds
  from `arch::entropy::draw` in a frame that never returns, before any
  protected frame is entered; a draw still `None` after `entropy::ATTEMPTS`
  refuses the boot. Linux's canary is per task; that gap is
  `issues/kernel/the-kernel-stack-canary-is-one-global-not-per-task.md`.
  **Exit**: the guest test `kernel_stack_canary` overflows a `boot-actuators`
  frame with zeros and asserts the panic names the stack protector;
  `static __stack_chk_guard: u64 = 0` passes the check, returns through a
  zeroed address and reds it. Each of S10's 64 boots prints the guard, and
  S10's rule holds for each of its 64 bits; a constant non-zero guard sets
  every bit in 0 or 64 boots and reds it.
- **S10 — Kernel ASLR, `Tier::Weekly` (`src/tiers.rs`).** Its first step
  times one `boot-actuators` boot of the TCG model and records 64 times that as
  the weekly cost. The loader, which builds the mapping the kernel starts in,
  draws the kernel image's virtual base and the direct map's base per boot
  and refuses the boot when its draw fails; the kernel receives both in its
  boot information, and `PHYS_OFFSET`'s two declarations
  (`kernel/src/mm/mod.rs`, `bootloader/src/main.rs`) become that one
  handed-over value. Each base's bits are stated against Linux's:
  `CONFIG_RANDOMIZE_BASE` puts the text in a 2 MiB slot of `KERNEL_IMAGE_SIZE`
  = 1 GiB (`arch/x86/include/asm/page_64_types.h:95-96`, at most 9 bits), and
  `CONFIG_RANDOMIZE_MEMORY` draws `page_offset_base` at PUD granularity
  (`arch/x86/mm/kaslr.c:146`). **Exit**: 64 QEMU boots each print both bases
  on a `boot-actuators` line; every claimed bit is set in between 12 and 52 of
  them (per-bit false-red probability 1.0e-7, exact binomial tail); a fixed
  base reds it.

## Decisions

- **IBPB is conditional**, Linux's default: `spectre_v2_user_select_mitigation`
  maps the default command to PRCTL, which enables `switch_mm_cond_ibpb`
  (`bugs.c:1512-1543`), and `arch/x86/mm/tlb.c:442-444` (`cond_mitigation`)
  issues IBPB on a switch between two processes only when either is flagged,
  printing "IBPB: conditional" (`bugs.c:3195-3205`). The flag is S6's.
- **Probes add no syscall.** They are compiled under `boot-actuators`.

## Exit

The T14's ToyOS boot prints, for each vulnerabilities file S0 captured,
exactly S0's Linux line. Where ToyOS mitigates differently, the line and why
it is not worse:

- `spectre_v1`: S0's line; ToyOS owes none of its swapgs barriers because it
  executes no `swapgs` (S4).
- `spectre_v2`: S0's line without its VM-exit clauses (`PBRSB-eIBRS: …`,
  `, KVM: …`); ToyOS runs no guest, so it has no VM exit to mitigate.
- `spec_store_bypass`: "Mitigation: Speculative Store Bypass disabled per
  program"; any program can be flagged in `system.toml`, where Linux's
  default mitigates only a process that asks by prctl.

Reading ARCH_CAPABILITIES as 0 instead of from 0x10A turns `spectre_v2` into
"Mitigation: Retpolines; …" and `gather_data_sampling` into "Vulnerable: No
microcode", and reds it. S1–S6 and S8–S10 are green, and S7 is closed by S0's
finding or by the owner.
