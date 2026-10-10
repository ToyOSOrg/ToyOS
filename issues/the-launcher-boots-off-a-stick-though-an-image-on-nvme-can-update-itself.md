---
status: open
kind: tooling
opened: 2026-10-10
---

# The launcher boots off a stick though an image on NVMe can update itself

`cargo run`'s machine (`src/qemu.rs`) boots its image off a USB stick beside
an NVMe disk, `target/nvme.img`. It did so because only a disk the kernel
drives could have its slots written; `update` now writes the idle slot through
diskserver's sessions, and `update_writes_the_idle_slot_through_the_block_service`
installs into a second slot on a guest booted off its NVMe disk. So the
development machine still boots through the kernel's USB storage path, which
usbd is to replace, and not through diskserver, which the T14 runs on once
ToyOS is installed on its internal disk.

**Exit**: `cargo run`'s machine boots its image off its NVMe disk, and
`update` run on it installs into its idle slot.

## Owner

The launcher, `src/qemu.rs`; unheld.
