---
status: open
kind: track
opened: 2026-09-29
---

# The kernel mitigates what Linux mitigates on the T14

Owner ruling: ToyOS also needs to be at least as secure as Linux on the same
hardware. The T14 is the reference machine this project already boots and
benches (`issues/hardware/the-t14-boots-toyos-unattended.md`), and Linux 6.8's
`/sys/devices/system/cpu/vulnerabilities/*` on it is the scoreboard this track
matches, line for line.

## What is true today

The kernel writes no CPU-vulnerability mitigation. There is no `IA32_SPEC_CTRL`
(0x48), `IA32_PRED_CMD`/IBPB (0x49), `IA32_ARCH_CAPABILITIES` (0x10A) or
`IA32_FLUSH_CMD` (0x10B) MSR access anywhere in `kernel/src/`, no `lfence`
speculation barrier, no `verw` buffer-clear, no retpoline codegen flag and no
`swapgs` timing mitigation (`rg` for each across `kernel/src/` and
`kernel/.cargo/config.toml` returns nothing but unrelated matches — a magic
number at `kernel/src/mm/alloc.rs:67` and a `RETF` opcode comment at
`kernel/src/arch/x86_64/smp.rs:465`). `EFER.NXE` is required on every CPU
(`kernel/src/arch/x86_64/control_regs.rs:80`); SMEP, SMAP and UMIP are taken
only when the CPU offers them, never required
(`kernel/src/arch/x86_64/control_regs.rs:60`, `CR4_OPTIONAL`). Every user
process loads at the same fixed address on every run:
`USER_VM_BASE = 0x100_0000_0000` (`kernel/src/loader/mod.rs:48`) and
`STACK_BASE = 0x00FF_FF80_0000` (`kernel/src/vma.rs:12`) are compile-time
constants, and `rebase_base` (`toyos-userbound/src/span.rs:60-63`) is a pure
function of them with no random input — there is no KASLR and no user-space
ASLR. The kernel's own `.cargo/config.toml` sets no stack-protector flag
(`kernel/.cargo/config.toml:1-3`); the rust fork's compiler carries
`-Zstack-protector` (`rust/compiler/rustc_session/src/options.rs:2843`,
codegen at `rust/compiler/rustc_codegen_llvm/src/attributes.rs:362`) and every
ToyOS target defaults `supports_stack_protector: true`
(`rust/compiler/rustc_target/src/spec/mod.rs:2974`, unset by
`rust/compiler/rustc_target/src/spec/base/toyos.rs`), so the capability exists
in the toolchain and is simply never turned on.

## Stages

Each stage names an exit a command can check. "QEMU" means the fast tier's
q35 model proves the code path exists and does not regress; "T14-only" means
the claim is about which mitigation the real silicon needs or reports, which
no emulator answers.

- **S0 — Evidence.** On the T14: save every
  `/sys/devices/system/cpu/vulnerabilities/*` file, the running microcode
  version, and `rdmsr` of 0x10A (`IA32_ARCH_CAPABILITIES`), 0x123
  (`IA32_TSX_CTRL` if present) and 0x48 (`IA32_SPEC_CTRL`, to see the reset
  value). On ToyOS booted on the same machine: log CPUID and 0x10A the same
  way (0x8B, the microcode-revision MSR, is read but never written). A human
  reads Intel's mitigation table row for the T14's model (family/model/stepping
  06_8C1, "Tiger Lake U 06_8C1" or as CPUID reports) against the saved BIOS/microcode
  version. **Exit**: a file in the T14 bench log names, per vulnerability,
  whether Linux mitigates it and how, and whether the shipped microcode is at
  or behind the revision that mitigation needs. T14-only — nothing here runs
  under QEMU.
- **S1 — Enumeration.** A kernel module reads `CPUID.(EAX=7,ECX=0):EDX` bits
  26 (IBRS/IBPB present), 27 (STIBP), 29 (SSBD) and 31 (SSBD enumerated via a
  different leaf on some parts — read the SDM table S0 pulled) and
  `IA32_ARCH_CAPABILITIES` (RDCL_NO, IBRS_ALL, RSBA, SKIP_L1DFL_VMENTRY,
  MDS_NO, TAA_NO, SBDR_SSDP_NO, FBSDP_NO, PSDP_NO, GDS_CTRL, GDS_NO), and
  writes one sysfs-shaped line per vulnerability, matching Linux's wording
  where the answer is "Not affected" or names the same mechanism. **Exit**:
  QEMU with `-cpu host,+arch-capabilities` reports the bits the host CPU has;
  a negative-control CPU model (e.g. `-cpu qemu64`, no `ARCH_CAPABILITIES`)
  reports every line as unmitigated-and-vulnerable, proving the enumeration
  is read from CPUID and not hardcoded. T14 confirms the real bit values S0
  recorded.
- **S2 — eIBRS and the GDS lock.** `IA32_SPEC_CTRL.IBRS` set once per CPU at
  the same site as the rest of the control-register declaration
  (`kernel/src/arch/x86_64/control_regs.rs`'s one-declaration pattern), and
  `IA32_MCU_OPT_CTRL`'s GDS lock bit set if `ARCH_CAPABILITIES.GDS_CTRL` is
  present, both asserted read-back on every AP the way `self_check` already
  asserts CR0/CR4/EFER. **Exit**: QEMU asserts the read-back matches the
  declaration on every CPU brought up by `-smp`. Whether eIBRS actually closes
  the branch-predictor channel is T14-only.
- **S3 — Retrograde state at the boundaries.** BHB (branch history buffer)
  clearing in `syscall_entry` when the CPU lacks eIBRS's hardware clearing;
  RSB (return stack buffer) refill in the context switch; one IBPB
  (`IA32_PRED_CMD`) write at `Cr3::activate` between two different processes.
  **Exit**: QEMU proves the instructions execute on every syscall entry and
  every process-to-process switch (a counter incremented at each site, read
  back through a test syscall) and that a same-process switch or fork does
  not issue the extra IBPB. Whether the RSB/BHB state that would have leaked
  is gone is a T14-only PoC (a Spectre-BTB gadget across the boundary,
  observed with and without the mitigation).
- **S4 — Spectre v1.** A `nospec` primitive (a serializing barrier after a
  bounds check before a speculatively-executed dependent load) applied at
  every syscall argument bounds check that gates an array or slice index; a
  ledger of every site it was applied to, and a lint that reds on a new
  bounds-gated index missing from the ledger. **Exit**: QEMU runs the existing
  syscall fuzzing suite unchanged with the primitive in place (no functional
  regression) and the lint reds when a planted new indexed access is left off
  the ledger. Efficacy — whether the barrier actually stops a cache-timing
  side channel — is T14-only, with a PoC gadget timed with and without it.
- **S5 — Indirect Target Selection (branch-history injection via indirect
  branch predictor state, ITS).** Retpoline-style thunks
  (`-Zretpoline-external-thunk`, `-Zfunction-return=thunk-extern`) for every
  indirect call and return in the kernel, with `core` and `alloc` rebuilt
  under the same flags. **Exit**: QEMU builds and boots with the flags on; a
  static scan of the built `kernel.elf` (the oracle) finds zero raw indirect
  `call`/`jmp`/`ret` outside the thunk implementations themselves. Whether the
  affected CPU family actually needs the thunks (vs. microcode-only mitigation)
  is read from S0's T14 evidence.
- **S6 — Speculative Store Bypass.** `IA32_SPEC_CTRL.SSBD` opt-in per process,
  matching Linux's default of leaving SSB unmitigated unless a process (via
  seccomp/prctl on Linux) asks for it — no process is charged the SSBD
  performance cost by default. **Exit**: QEMU proves the bit is clear for an
  ordinary process and set for one that opts in, read back through
  `IA32_SPEC_CTRL`.
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
- **S8 — User-space ASLR.** Every process's image base and initial stack base
  are drawn from a per-spawn random source (`SYS_RANDOM`, already implemented)
  instead of the fixed `USER_VM_BASE`/`STACK_BASE` constants, still satisfying
  `rebase_base`'s `in_user_half` bound and 2 MiB alignment. **Exit**: a guest
  test spawns the same binary twice and asserts the two runs report different
  image bases (e.g. through `SYS_SYSINFO` or a probe syscall) while both stay
  inside the user half and both run correctly. QEMU-provable in full; the T14
  adds nothing this property needs.
- **S9 — A compiler stack protector for the kernel.** `kernel/.cargo/config.toml`
  gains `-Zstack-protector=strong` (or the strategy S0/S1's threat model
  picks), building `core` under the same flag. **Exit**: a guest test plants a
  deliberate stack buffer overflow in a test-only kernel path and asserts the
  kernel panics naming the stack-protector check, not a silent corruption or
  an unrelated fault. QEMU-provable in full.

## Decisions

- **IBPB between processes, not between threads of one process** — matches
  Linux's own default (`spectre_v2` mitigation issues IBPB only across a
  security-domain boundary, i.e. a different `mm`, not on every context
  switch). S3 states this as the boundary condition its guest test checks.
- **S7 (microcode loading) is parked for the owner**, per the CLAUDE.md
  citation above — it is the one stage this track cannot rule on by itself,
  and it is needed at all only if S0's BIOS-revision read comes back behind
  Linux's.

## Exit

On the T14, every `/sys/devices/system/cpu/vulnerabilities/*` line Linux
reports has a ToyOS line that classifies the same vulnerability the same way
(mitigated by the same mechanism, or "Not affected" for the same CPUID-derived
reason), and the LLVM toolchain bar (`issues/build/toyos-builds-itself.md`)
is re-run in full with every mitigation in this track turned on, green.
