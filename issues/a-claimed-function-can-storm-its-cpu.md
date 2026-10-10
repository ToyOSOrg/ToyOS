---
status: open
kind: defect
opened: 2026-10-04
---

# A claimed function can storm its CPU

A function a process drives raises its claim slot's interrupt on cpu0 for
every message it sends — on x86-64 the slot's vector
(`kernel/src/arch/x86_64/idt/user_dev.rs`), on AArch64 the slot's LPI through
the ITS (`kernel/src/arch/aarch64/irqchip/its.rs`) — and `pcidev::isr`
(`kernel/src/pcidev/mod.rs`) takes each one; nothing bounds how often. Its
holder programs the device through the BARs it maps, so a hostile or broken
holder can make the device send messages as fast as the bus carries
them, and each one is an interrupt cpu0 takes before anything else it runs.
`issues/an-xhci-storm-starves-the-cpu-that-takes-it.md` measured what
that does to a CPU for a kernel driver's source; a claim is the same source
with its holder outside the kernel.

The lever is the slot's own translation: on x86-64 its remapping entry, which
made not present stops the function's messages at the unit; on AArch64 its
LPI's configuration byte, which cleared and followed by an `INV` disables
the LPI at the redistributor. Masking through the device would need the
holder's cooperation. What rate is a storm is not decided, and a number
chosen without a measurement behind it is the silent decision this file
exists to refuse.

Owner: the IOMMU track. Exit: a claimed function's messages past a bound the
kernel derives and states are stopped at its entry and recorded against its
claim, and a T14 row with a holder that provokes them shows cpu0's timer and
other sources still served.
