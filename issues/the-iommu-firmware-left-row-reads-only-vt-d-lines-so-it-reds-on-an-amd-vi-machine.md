---
status: open
kind: defect
opened: 2026-10-12
---

# The `iommu_firmware_left` metal row reads only VT-d lines, so it reds on an AMD-Vi machine

The row rides the `shared-debug` boot, whose `SELFTEST_PARAMS` arm
`iommu-firmware-left` on whichever bench runs it. Its judge,
`iommu_firmware_left` in `tests/toyos.rs`, finds the units it judges by the
lines carrying ` translating gsts=`, which only
`kernel/src/arch/x86_64/vtd/mod.rs` prints, and reds when it finds none: "no
unit was programmed, so nothing was handed over".

An AMD-Vi machine has no VT-d unit and programs no unit. Under the actuator,
`kernel/src/arch/x86_64/amdvi/mod.rs` says what it switched off in lines of
its own: `iommu: amdvi unit<N> was handed over with <field> on; it goes off`
for `CmdBufEn`, `EventLogEn` and `IommuEn`, then `iommu: amdvi unit<N> @<base>
switched off`. The judge reads none of them, so a boot that switched every
AMD-Vi unit off reds it.

Evidence: the AMD bench's `shared-debug` boot at #873's head logs `ACPI: DMAR
not read: Absent` and one AMD-Vi unit
([flash log](https://github.com/ToyOSOrg/evidence/blob/ffa5669481d76ad92545bc6f5cc89d3a53b54cbc/flash/amd-session2/head-amd-shared-debug-flash.log#L107-L198)).
That boot panics in the actuator's command ring first (`amdvi/mod.rs:325`), so
the judge's red is read off its code rather than off a run: once the boot
completes, no line of its log carries ` translating gsts=`.

Owner: the orchestrator's AMD laptop track, whose stage 1 (#837) gave
`iommu-firmware-left` its AMD-Vi arm and left this judge reading VT-d alone.
The judge is the metal harness's, in the `METAL` table of `tests/toyos.rs`.

**Exit:** the `iommu_firmware_left` row passes the AMD bench's `shared-debug`
boot on each AMD-Vi unit's three handed-over lines and its switched-off line,
reds that boot with any of them missing, and still passes the T14's.
