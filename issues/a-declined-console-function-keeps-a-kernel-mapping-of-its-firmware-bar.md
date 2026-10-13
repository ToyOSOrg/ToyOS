---
status: assigned
kind: defect
opened: 2026-10-13
---

# A declined console function keeps a kernel mapping of its firmware BAR

Held by the orchestrator.

The kernel's console reads every virtio console function's device features
before it picks one (`kernel/src/drivers/virtio_console.rs`,
`VirtioDevice::offer` in `kernel/src/drivers/virtio.rs`). That read maps the
function's capability windows through `map_mmio`, at the BAR addresses
firmware assigned. A function the console declines is reset and left to a
claim, and its windows stay mapped in the kernel's direct map and listed in
x86_64's driven windows (`kernel/src/arch/x86_64/paging.rs`), which nothing
removes. `pcidev` then moves that BAR when a process claims the function, so
the kernel keeps a mapping of an address range the function no longer
decodes. Nothing reads through it today.

Virtio 1.2 §4.1.4.9's PCI configuration access capability reaches a BAR's
registers through configuration space with nothing mapped.

**Exit condition.** A function the console declines leaves no kernel mapping
and no driven window behind.
