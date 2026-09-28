---
status: open
kind: defect
opened: 2026-09-27
---

# A window's buffer is read while its client writes it

Each window has one region, which the client draws into and the compositor
blits from (`render::draw_window` in `userland/compositor/src/render.rs`). The
compositor reads it as a `&[u8]` while the client may be writing it: a data
race in Rust's model, and on the panel a frame whose pixels come from two of
the client's frames.

Owner: the compositor's window blit, `render::draw_window` in
`userland/compositor/src/render.rs`.

**Exit**: the compositor makes two buffers per window, a client hands one over
with its present and gets it back on release, and the compositor never reads a
buffer the client holds.
