---
status: open
kind: defect
opened: 2026-10-12
---

# The x86 clock calibrates against an HPET, or the boot panics

`arch::boot::clock` (`kernel/src/arch/x86_64/boot.rs`) takes the HPET's base
from its ACPI table with `.expect("ACPI: HPET not found")`, and every rate the
kernel keeps descends from that one measurement: the TSC's
(`kernel/src/arch/x86_64/hpet.rs`) and the local APIC timer's, calibrated
against the TSC (`apic::init_timer`). CPUID 15H/16H is read after it, as a
cross-check only. A machine whose firmware publishes no `HPET` table stops
there, before a timer ticks, and before PCI is enumerated, so a console that
is a virtio function never carries the panic.

QEMU's q35 has one. Of the other targets, read from their sources and not
booted:

| | HPET table | ACPI PM timer | TSC rate the platform states |
|---|---|---|---|
| Cloud Hypervisor | none (`vmm/src/acpi.rs` builds no `HPET`) | `X_PM_TMR_BLK` | CPUID `0x40000010` EAX, in kHz, where the host's TSC is invariant (`arch/src/x86_64/mod.rs`) |
| crosvm | none (no HPET device in `devices/src`) | none ("not supported", `x86_64/src/acpi.rs`) | CPUID 15H, where a calibrated leaf is configured (`x86_64/src/cpuid.rs`) |
| VirtualBox | only with `--x86-hpet on`, default off (`HpetEnabled`, `DevACPI.cpp`) | yes | not read |
| bhyve | yes (`usr.sbin/bhyve/acpi.c`) | yes | not read |

So Cloud Hypervisor, crosvm and VirtualBox without `--x86-hpet on` all end at
this `expect`.

What a machine states instead, by its own documents: the ACPI PM timer, fixed
hardware at 3.579545 MHz (ACPI 6.5 §4.8.3.3), which q35, Cloud Hypervisor,
VirtualBox and bhyve publish; CPUID 15H's crystal ratio (Intel SDM Vol. 2A),
which the kernel already reads through `toyos_tsc::stated_hz` and which no AMD
part states (`issues/the-loaders-lines-carry-no-time-on-an-x86-cpu-that-states-no-tsc-rate.md`);
and a hypervisor's timing leaf `0x40000010`. crosvm without a configured leaf
states none of them.

**Owner**: whoever holds `issues/toyos-runs-in-any-open-source-hypervisor.md`.

**Exit**: a q35 boot with `-machine hpet=off` reaches `Boot: complete`, its
`TSC:` line naming the source its rate came from; and a machine that states no
source is refused by a line that names each one it looked for.
