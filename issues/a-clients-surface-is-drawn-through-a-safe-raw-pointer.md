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

The same constructors are unsound over private memory too.
`system/compositor/src/render.rs`'s `BackBuffer` draws through a
`Framebuffer` over its own `Vec`'s pointer while `BackBuffer::region` lends
`&self.pixels` of the same bytes. The compositor forbids `unsafe`, so the
constructors cannot simply become `unsafe` fns: what it needs is a surface
that owns or borrows the memory it draws into.

Owner: `sdk/window`.

**Exit**: no safe `Framebuffer` or `Screen` constructor takes a raw pointer:
one owns or borrows the memory it draws into, and a raw one is `unsafe` with
its preconditions stated; and a client's buffer has one writable mapping, the
client's, so drawing into it with plain accesses is sound, or the client draws
into memory of its own and copies.
