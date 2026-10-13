---
status: assigned
kind: defect
opened: 2026-10-13
---

# A `cargo run` boot claims every one of pcidev's slots

Held by the orchestrator.

`images/system.toml`'s rows claim four PCI functions on `cargo run`'s q35:
virtio-sound (`soundserver`), virtio-net (`netstack`), the NVMe controller
(`diskserver`) and the clipboard's virtio-serial function (`vdagent`). That is
every one of `pcidev::MAX_FUNCTIONS` (`kernel/src/pcidev/mod.rs`), so a fifth
claim on that machine is refused `Exhausted`.

None of them goes back: a virtio function on q35's root bus publishes no power
management capability and no PCI Express capability, so it has no
function-level reset, and an ended holder leaves its slot as that function's
residue (`kernel/pci/src/slot.rs`).

More slots reach both architectures' interrupt tables, which declare one
interrupt per slot (`kernel/src/arch/x86_64/idt/user_dev.rs`,
`kernel/src/arch/aarch64/irqchip/its.rs`).

**Exit condition.** A `cargo run` boot leaves a claim slot free.
