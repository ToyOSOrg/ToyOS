---
status: open
kind: defect
opened: 2026-10-10
---

# The boot volume's server holds a claim that writes

The boot volume is the ESP firmware loaded the loader from (`Role::Boot`,
`kernel/src/gpt.rs`): on the image the build writes it carries the loader and
`toyos/log.guid`, which the loader reads beside itself; the kernel and its
parameter are on the booted slot's volume (`Role::Slot`), which no file server
serves (`create_esp_volume` and `create_slot_volume`, `build/image.rs`).

On a disk the kernel drives — the boot stick, until usbd serves it — the
supervisor endows the boot role's file server a partition claim on the ESP
(`storage_endowment`, `system/supervisor/src/main.rs`). A claim carries
`Rights::WRITE` and no `DUP` (`initial_rights`, `kernel/src/object/ops.rs`), so
nothing can hand that server a duplicate narrowed to reading: the loader
firmware runs stays unwritten only by that server's promise to mount the ESP
read-only (`fat_on(disk, writable)`, `system/fileserver/src/main.rs`). On a
disk the block service serves, the same server's grant does not write, and
diskserver answers a write through it `ReadOnly` before the device sees it
(`system/diskserver/src/main.rs`'s module header).

**Exit**: the boot volume's server on the boot stick holds a partition it
cannot write — a read-only session once usbd serves the stick, or a claim the
kernel mints without `WRITE` — and a test's write through it is refused.

## Owner

The usbd cutover of `issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`; unheld.
