---
status: open
kind: defect
opened: 2026-09-26
---

# softbuffer reads a ToyOS window behind the lock winit holds it with

The winit backend keeps each `window::Window` in an `Arc<Mutex<_>>`, and the
event loop takes the lock to read events: `poll_event(&mut self)` replaces the
window's shared buffer on a resize. The raw window handle winit hands out is
the address of that `Window`, and softbuffer's ToyOS backend
(`src/backends/toyos.rs` on `ToyOSOrg/softbuffer`) dereferences it without the
lock to blit and present. A surface presented from a thread other than the
event loop's therefore races the resize: a present into the buffer the loop
just dropped. The toolkits in the tree present from the loop's own thread, so
nothing here has seen it.

Exit condition: the handle names something both sides reach through one
lock, or `toyos-window` makes the buffer swap safe against a concurrent
present, with a guest test that presents from another thread across resizes.
