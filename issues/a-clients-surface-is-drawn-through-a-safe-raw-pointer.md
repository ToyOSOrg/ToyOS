---
status: open
kind: defect
opened: 2026-10-27
---

# A client's surface is drawn through a safe raw pointer into memory the compositor maps writable

`window::Framebuffer::new` and `window::Screen::new` (`sdk/window/src/framebuffer.rs`)
are safe constructors over any `*mut u8`, and every later draw reads and
writes through it. `Window::framebuffer` and `Window::screen` build one over
the client's buffer, which the compositor made and still maps writable, so the
client's plain loads and stores are sound only while the compositor never
writes that buffer. The compositor does not today, but nothing but its code
says so: the kernel cannot share a region read-only (`sys_shm_map` maps every
writable region read-write), so no type can say that the client's mapping is
the region's only writer.

The compositor's side is sound: it reads a client's buffer only by copies
(`Framebuffer::blit_shared`, `toyos::shm::Bytes`).

The drivers that hand a received frame on as a slice (`system/netstack/src/i219.rs`,
`virtio_net.rs`, `system/vdagent/src/channel.rs`) rest on the same kind of
promise: the device's descriptor says it has finished with the buffer, which
is the device's protocol and not a type.

Owner: `sdk/window`.

**Exit**: `Framebuffer::new` and `Screen::new` are `unsafe` with their
preconditions stated, or take the mapping they draw into; and a client's
buffer has one writable mapping, the client's, so drawing into it with plain
accesses is sound, or the client draws into memory of its own and copies.
