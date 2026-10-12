---
status: open
kind: defect
opened: 2026-10-11
---

# The ARM desktop starts with no keyboard or mouse

An AArch64 boot of `images/system.toml` under QEMU `virt` reaches the
supervisor and initializes virtio-gpu, but the compositor exits 101 at
`system/compositor/src/session.rs` because its keyboard endowment is absent.
The supervisor reports both keyboard and mouse absent. This was read in
the owner's boot of `57616a067` and reproduced with the shipping image.

The launcher attaches a USB keyboard and tablet to its xHCI. The kernel's
xHCI driver refuses to arm on AArch64: `arch/aarch64/irqchip/its.rs::msi`
refuses kernel drivers. Enabling the ITS makes PCI claims possible but
does not give that driver an interrupt. `system/usbd` currently enumerates
devices without binding their classes and is not started by the shipped
manifest. There is no virtio-input driver.

Owner: `issues/toyos-runs-on-arm64.md`, stages 6 and 7, with the userland
USB move in `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`.
DMA and interrupt isolation remain required.

Exit: an ARM boot of the shipped manifest supplies real keyboard and mouse
input to the compositor; injected key transitions and pointer events reach
the desktop and an application's window in a test that fails when delivery
is removed. The compositor starts and presents a frame without the missing
keyboard panic.
