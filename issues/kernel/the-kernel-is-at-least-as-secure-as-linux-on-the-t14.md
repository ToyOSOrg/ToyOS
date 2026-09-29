---
status: open
kind: track
opened: 2026-09-29
---

# The kernel is at least as secure as Linux on the T14

Owner ruling: parity with the Linux the T14
(`issues/hardware/the-t14-boots-toyos-unattended.md`) runs is the floor, and
the T14 also gets every security feature its CPU and platform support. That
Linux is tag `Ubuntu-6.8.0-142.142` (53e5d07aac028a1523ab0b115f079d6d1bc831ef)
of `https://git.launchpad.net/~ubuntu-kernel/ubuntu/+source/linux/+git/noble`,
config sha256 3b8533dd9d235ca634ac58f82c5ce1ee35f12ef620693e17033184d2c9ca5890.
A hardening default its config sets and no vulnerabilities line reports is
closed by the stage naming it or by a defect the Exit names; ToyOS already
meets `INIT_ON_ALLOC_DEFAULT_ON`, `SCHED_STACK_END_CHECK`, `HARDENED_USERCOPY`
and `X86_UMIP`, Rust meets `INIT_STACK_ALL_ZERO`, `FORTIFY_SOURCE` and
`UBSAN_*`, ToyOS has no modules, kexec, BPF or vsyscall page, and
`SHUFFLE_PAGE_ALLOCATOR` is off by default. A QEMU test asserts wiring, S1
over the facts the guest reports. A probe is a `boot-actuators` arm or a
`test-actuators` `SYS_DEBUG` action, never a syscall. A stage's oracle is the
T14's CPU unless it names another.

- **S0 — Evidence.** Exit: the T14's and the TCG model's captures under that
  Linux, committed as S1's fixtures, and no Linux image; before the wipe it
  adds CPUID 5, 0x19 and 0x80000001, MSR 0xCF, the split-lock line and the
  config's IBT and PKU options. Mutation: a config hash off by one byte is
  refused. Oracle: that Linux. Ubuntu leaves the T14 only after this and P0 of
  `issues/kernel/toyos-uses-what-the-t14s-hardware-offers-for-speed.md`.
- **S1 — The decision**, host-tested, from a CPU's facts to each
  vulnerabilities line and mitigation. Exit: S0's facts give S0's lines.
  Mutation: `GDS` deleted from `cpu_vuln_blacklist`'s TIGERLAKE_L row.
  Oracle: S0's lines.
- **S2 — SPEC_CTRL and GDS.** Exit, T14: every CPU's 0x48 is S0's read less
  SSBD, and a probe sets `GDS_MITG_DIS` and reads it clear after the kernel's
  write. Mutation: the clear deleted. Oracle: S0's reads.
- **S3 — BHB clear, RSB fill, IBPB conditional on S6's flag.** Exit: a gate
  matches `kernel.elf` against `clear_bhb_loop` and `__FILL_RETURN_BUFFER`;
  probes count a clear per syscall and two IBPBs over B, C, idle, B, A, idle,
  A, B on the T14, and a fill per switch under TCG. Mutation: either sequence
  edited, the clear's condition inverted, no fill, IBPB decided by the
  previous root. Oracle: Linux's sequences.
- **S4 — Spectre v1**: SMAP required, `lfence` and mask at `user_ptr`'s range
  checks. Exit: a boot without SMAP is refused by name; on the T14 a gadget
  through each site leaks a planted byte in at most 16 of 1000 trials.
  Mutation: SMAP optional; a site's fence or mask gone must exceed 16.
- **S5 — Thunks for every indirect branch and return, and `SLS`.** Exit: a
  gate finds no raw indirect branch or `ret` outside thunks and entry, `int3`
  after each there, each ITS thunk's branch at `addr & 63 >= 32`; boots
  compare live thunks with S1's. Mutation: a branch at a line start, an `int3`
  gone, the ITS thunk everywhere. Oracle: `indirect-target-selection.rst`.
- **S6 — SSBD per program flagged in `system.toml`**, inherited at spawn.
  Exit, T14: it reads clear, set, and set in a flagged process's child.
  Mutation: no inheritance.
- **S7 — Microcode loading, the owner's**: it is firmware run on the CPU,
  which root `CLAUDE.md` does not carve out. Exit: S0 shows the BIOS at
  Linux's fixing revision, or the owner rules.
- **S8 — User ASLR, `ARCH_MMAP_RND_BITS`.** Exit: over 256 spawns each claimed
  bit of the three bases is set 88 to 168 times. Mutation: `BASE + n * 2
  MiB`, a constant seed. Oracle: Linux's 32 bits for image and mmap and 22
  for the stack; a shortfall is a defect.
- **S9 — `STACKPROTECTOR_STRONG`** at `%gs:N`, by LLVM and rustc changes the
  orchestrator admitted as upstream-quality cross-platform options. Exit:
  `stack-protector-3.ll` and a rustc assembly test see `%gs:<offset>` and no
  `__stack_chk_guard`; `kernel_stack_canary` panics on B's guard in A's
  frame. Mutation: the old lowering, the flags dropped, `context_switch`'s
  write gone, a constant guard. Oracle: FileCheck, and the CPU's compare.
- **S10 — `RANDOMIZE_BASE`, `RANDOMIZE_MEMORY`, `Tier::Weekly`.** Exit: 64
  boots set each claimed bit 12 to 52 times, S9's guard and S8's first spawn
  included. Mutation: a fixed base. Oracle: Linux's at most 9 bits of text
  slot and a PUD-granular direct map.
- **S11 — `X86_USER_SHADOW_STACK`.** Owner ruling: the loader refuses every
  program and library without `GNU_PROPERTY_X86_FEATURE_1_SHSTK`, on every
  CPU. Exit: `unmarked_object_refused` under TCG; a sysroot link with an
  unmarked input reds; on the T14 a forged return is `#CP` in two threads, a
  caught panic unwinds, a store to the shadow stack is `#PF`, and `user_ptr`
  refuses a `read` into it with `BadAddress`. Mutation: the check gone,
  `SH_STK_EN` clear, one shadow stack for two threads, no `incssp`, the stack
  mapped writable or faulted in as CoW.
- **S12 — `X86_KERNEL_IBT`**, unset in the config. Exit: a gate finds
  `endbr64` at every IDT and `IA32_LSTAR` target and no `notrack`; on the T14
  a call to a function without one is `#CP`. Mutation: `syscall_entry`'s
  `endbr64` gone, `ENDBR_EN` clear.
- **S13 — Kernel shadow stack**, which Linux lacks; CET_SSS, CPUID.(7,1):EDX
  bit 18. Exit, T14: a forged kernel return is `#CP`, `kernel_stack_canary`
  runs under it, a `DirectMap` store to a shadow stack faults. Mutation:
  `SH_STK_EN` clear, SSP not switched, the frame in the writable direct map.
- **S14 — `X86_INTEL_MEMORY_PROTECTION_KEYS`**, after P1; owner ruling: its
  ABI is approved in principle. Exit, TCG `+pku` and T14: a write under a key
  the thread denied is `#PF` with PK, a sibling's is not, and `window` refuses a
  `read` into it. Mutation: no key in the entry, one PKRU for all threads, no
  key check in `window`. Oracle: TCG and the T14 on SDM Vol. 3A §5.6.2.
- **S15 — Key Locker**, after the speed track's Pin; CPUID.19H:EBX bits 0 and
  4, ECX bit 1. Exit, T14: a handle wrapped on one pinned CPU gives FIPS-197
  C.1's ciphertext on another. Mutation: an IWKey per CPU. Oracle: FIPS-197.
- **S16 — Split-lock disable**, Linux's default without a config option; a
  split lock ends a ToyOS process where Linux warns. Exit, T14: a `lock add`
  across a line is ended by name. Mutation: `MSR_MEMORY_CTRL` bit 29 clear.

**Exit.** The T14's ToyOS boot prints S0's Linux line for each
vulnerabilities file, less `spectre_v1`'s swapgs barriers while ToyOS runs
no `swapgs` and `spectre_v2`'s VM-exit clauses, since it runs no guest;
`spec_store_bypass` reads "Mitigation: Speculative Store Bypass disabled per
program". Mutation: ARCH_CAPABILITIES read as 0. Every stage is green or
closed, and so is every defect the hardening defaults name:
`issues/boot-media/the-loader-never-sets-the-firmwares-memory-overwrite-request.md`,
`issues/kernel/every-syscall-runs-at-one-kernel-stack-offset.md`,
`issues/kernel/kernel-functions-return-with-their-used-registers-intact.md`,
`issues/kernel/a-threads-kernel-stack-has-no-guard-page.md`,
`issues/kernel/kernel-text-is-writable-and-every-kernel-page-executable.md`,
`issues/kernel/the-kernel-heap-has-none-of-slubs-hardening.md`,
`issues/kernel/tsx-stays-as-firmware-left-it.md`,
`issues/kernel/a-device-without-a-domain-of-its-own-reaches-all-memory.md`.
