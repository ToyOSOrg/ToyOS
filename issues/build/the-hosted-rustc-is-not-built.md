---
status: open
kind: track
opened: 2026-09-29
---

# The hosted rustc is not built

Nothing builds the ToyOS-hosted rustc (`x86_64-unknown-toyos`, Cranelift) any
more, and no image or release carries one. It was built in place in the
primary's `rust/build/` on every compiler change, against the ABI the primary
had at that moment and no other, and no image could ship it: every mode's
config that set `hosted-rustc` was refused until the compiler's licences are
read. The toolchain became a store of keyed products (`src/store.rs`), and a
compiler that links a ToyOS std depends on the ABI trees, so it is a product of
its own, keyed like a sysroot.

Exit: a store product keyed on a compiler and the ABI trees builds the hosted
rustc, an image carries it, and a guest test compiles and runs a program with it
(`issues/build/toyos-builds-itself.md`, M3;
`issues/build/the-hosted-rustc-names-a-linker-toyos-does-not-have.md`).
