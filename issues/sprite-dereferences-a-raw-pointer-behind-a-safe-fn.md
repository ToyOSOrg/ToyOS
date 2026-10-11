---
status: open
kind: defect
opened: 2026-10-11
---

# Sprite dereferences a raw pointer behind a safe fn

`Sprite::draw` (`system/compositor/sprite/src/lib.rs`) is a safe `pub fn`
that takes the destination as `dst: *mut u8` with a stride, a width and a
height, and writes through `dst.add(..)` in an `unsafe` block. Nothing ties
the pointer to the dimensions it is given, so a safe caller can make it
write anywhere. The compositor, which is `#![forbid(unsafe_code)]`, reaches
its back buffer's raw pointer through it (`draw_icon_centered`,
`system/compositor/src/render.rs`).

Clippy says so. Once a crate nested in a program became a host member,
the workspace's clippy shapes linted `sprite` for the first time, and
both refused it with `-D warnings`:

- `clippy::not_unsafe_ptr_arg_deref` at `draw`'s first write;
- `clippy::too_many_arguments` (8/7) on `draw`.

It had never been linted: as `userland/sprite` it held no test, so the
userland survey gated it nowhere. So that the move left nothing less
checked than before, `build/hostws.rs`'s `guest_built` names
`system/compositor/sprite`, which keeps it out of the host members.

**Owner:** the compositor.
**Exit:** `draw` writes only through a destination that carries its own
bounds, `system/compositor/sprite` leaves `guest_built`'s list, and the
workspace's clippy shapes pass it.
