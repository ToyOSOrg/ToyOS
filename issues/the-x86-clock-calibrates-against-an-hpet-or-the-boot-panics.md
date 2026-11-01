---
status: open
kind: defect
opened: 2026-10-12
---

# The x86 clock calibrates against an HPET, or the boot panics

`arch::boot::clock` (`kernel/src/arch/x86_64/boot.rs`) takes the HPET's base
from its ACPI table with `.expect("ACPI: HPET not found")`, and the clock
needs it whatever the CPU states: it measures the TSC's rate against the HPET
where CPUID 15H states none, and holds the TSC to it while it runs, a stated
rate too (`kernel/src/arch/x86_64/clocksource.rs`); the local APIC timer is
calibrated against the TSC (`apic::init_timer`). A machine whose firmware publishes no `HPET` table stops
there, before a timer ticks, and before PCI is enumerated, so a console that
is a virtio function never carries the panic.

QEMU's q35 has one. Of the other targets, read and not booted:

| | HPET table | ACPI PM timer | TSC rate the platform states |
|---|---|---|---|
| Cloud Hypervisor | none | none: an `X_PM_TMR_BLK` in a FADT that sets `HW_REDUCED_ACPI`, below | CPUID `0x40000010` EAX, in kHz, where the host's TSC is invariant |
| crosvm | none | none | CPUID 15H, where a calibrated leaf is configured |
| VirtualBox | only with `--x86-hpet on`, default off (`VBoxManage modifyvm`) | yes | not read |
| bhyve | yes | yes | not read |

So Cloud Hypervisor, crosvm and VirtualBox without `--x86-hpet on` all end at
this `expect`.

What a machine states instead, by its own documents: the ACPI PM timer, fixed
hardware at 3.579545 MHz (ACPI 6.5 §4.8.3.3), which q35, VirtualBox and bhyve
publish; CPUID 15H's crystal ratio (Intel SDM Vol. 1, 253665-093US, Table 21-53),
which the kernel already reads through `toyos_tsc::stated_hz` and which no AMD
part states (`issues/the-loaders-lines-carry-no-time-on-an-x86-cpu-that-states-no-tsc-rate.md`);
and a hypervisor's timing leaf `0x40000010`. crosvm without a configured leaf
states none of them. Cloud Hypervisor states the leaf alone: its FADT writes
`X_PM_TMR_BLK` (offset 208) but sets `HW_REDUCED_ACPI`, under which OSPM
"will ignore fields related to the ACPI HW register interface: Fields at
offsets 46 through 108 and 148 through 232" (ACPI 6.5 §5.2.9), the fixed
hardware being "not implemented" there (§4.1); `toyos_acpi::fixed_hardware`
refuses such a FADT whole (`lib/acpi/src/fadt.rs`). These ACPI sections are as
a third-party copy of ACPI 6.6 numbers them; 6.5 itself is unread.

**Owner**: whoever holds `issues/toyos-runs-in-any-open-source-hypervisor.md`.

**Exit**: a q35 boot with `-machine hpet=off` reaches `Boot: complete`, its
`clock:` line naming the source its rate came from and the reference that
holds it; and a machine that states no source is refused by a line that names
each one it looked for.
