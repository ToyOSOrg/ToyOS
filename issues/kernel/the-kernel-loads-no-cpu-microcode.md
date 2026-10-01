---
status: open
kind: defect
opened: 2026-09-29
---

# The kernel loads no CPU microcode

The kernel is to load CPU microcode signed by the CPU's maker, pinned by
version and hash the way vendor device firmware is (owner, 2026-09-30). It
loads none, so a CPU whose BIOS ships stale microcode stays below Linux at
`Ubuntu-6.8.0-142.142` on every line that rests on microcode.

Root `CLAUDE.md`'s firmware rule admits this microcode once PR #636 lands.

**Exit**: the kernel loads current microcode early on every CPU, at least as
current as Linux's.

`toyos-microcode` validates an Intel update file and picks the update for one
CPU; nothing calls it. The T14's eight CPUs run 0xbe from its firmware, which
is Intel's newest for them at `microcode-20260925`, so a load there is a no-op.
Its `IA32_PLATFORM_ID` is uncaptured: platform 7 is inferred from the one
platform that file names.

**Where.** The kernel, on every CPU, from a file it embeds per CPU ToyOS
supports on metal. Not the loader: it runs on the BSP alone, reaching the APs
takes EFI MP Services, and the kernel reads every CPU's revision anyway. In
`percpu::init_bsp` after `idt::init` and in `percpu::init_ap` after
`control_regs::init`, before `fpu::init` on each: after the IDT, so a load that
faults reports on this kernel's channels; before anything acts on an
enumeration an update changes (CPUID.7.0:EDX, `IA32_ARCH_CAPABILITIES`, RTM
and HLE); and before `ROSTER.echo`, so `boot_aps` starting one AP at a time is
the serialisation per core that SDM Vol. 3A §12.11.6.3 asks. A CPU with
CPUID.1:ECX[31] set loads nothing, as Linux does, and `IA32_PLATFORM_ID` is
read on a GenuineIntel CPU only. The update data starts 16-byte aligned (§12.11.6),
and the trigger is an `asm!` of its own: `cpu::wrmsr` is `nomem`, and the CPU
reads the update through this write. INIT keeps an update; a hard reset clears
it (§12.11.6.1).

**Verified.** After the trigger the CPU writes 0 to `IA32_BIOS_SIGN_ID`, runs
CPUID.01H, and panics unless it reads back the update's revision (Example
12-10). After `boot_aps`, CPUs that report different revisions panic the boot.
Each CPU logs its platform, the revision it found and the one it runs.

**AMD.** linux-firmware's `amd-ucode/microcode_amd_fam{17,19,1a}h.bin` is a
container (magic 0x00414d44): an equivalence table from CPUID.01H:EAX to a
processor ID, then patches. MSR C001_0020 takes a patch's address, and MSR 8B
must then read back its patch ID; family 0x17 also invalidates the patch's
pages (Linux `amd.c`, `__apply_microcode_amd`). On families 0x17, 0x19 and part
of 0x1a below a per-CPU cutoff revision the CPU's own signature check is broken
(EntrySign; `cpu_has_entrysign`, `need_sha_check`), so the hash pin is the only
check, as `amd_shas.c` is Linux's. linux-firmware's `LICENSE.amd-ucode` is
unread. ToyOS has no AMD metal: the nightly's EPYCs are KVM guests, which load
nothing.

**Licence.** `LicenseRef-Intel-Microcode` is in no `ALLOWED` row of
`src/licence.rs`, so the commit that embeds the file reds the licence gate
until an exception scoped to CPU microcode admits it. Intel's first condition
puts the notice `NOTICE` quotes into that image.

Each step's exit, in the testing ladder's order:

- host: the step's decisions are pure in `toyos-microcode`, each refused by
  name and each red under its mutation: a hypervisor or a vendor no file
  covers, `select`, a read-back that is not the update's revision, CPUs that
  disagree;
- metal: a T14 boot logs platform 7 and 0xbe current on all eight CPUs; a
  `boot-actuators` arm that raises only the header's revision to 0xbf and
  reseals its checksum stops that boot at the read-back or at a fault, since
  Intel's payload still says 0xbe;
- metal, on no machine ToyOS has: the load arm, on a CPU whose firmware runs
  older microcode than the embedded file; AMD's loader, on AMD metal;
- guest: every CPU logs why it loads nothing, a hypervisor under KVM and a
  vendor no file covers under TCG's `AuthenticAMD` `qemu64`, and a guest test
  asserts the line.
