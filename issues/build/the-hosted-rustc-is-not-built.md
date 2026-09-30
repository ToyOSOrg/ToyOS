---
status: open
kind: track
opened: 2026-09-29
---

# The hosted rustc is not built

Nothing builds the ToyOS-hosted rustc (`x86_64-unknown-toyos`, Cranelift) any
more, and no image or release carries one.

Exit: a store product keyed on a compiler and the ABI trees builds the hosted
rustc, the licences of the compiler it ships are read, an image carries it, and
a guest test compiles and runs a program with it
(`issues/build/toyos-builds-itself.md`, M3;
`issues/build/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`).
