---
status: open
kind: defect
opened: 2026-10-03
---

# One checkout of one tree builds two kernels

A kernel's bytes depend on what its target directory built before. Measured on
`wt/toyos-ps2`, the tree clean each time, of the x86-64 kernel with
`boot-actuators,test-actuators`, which is built from the same sources at both
heads:

- At `66798c7ed`, read back out of a staged metal image (`toyos/kernel.elf` on
  slot A's volume): 3514896 bytes and sha256
  `2593c71c4c86f3c03815f08afc6a89eb9832c351e99376ffb4aab7858c8f83c2` when the
  build before it had one expression of `kernel/src/arch/x86_64/pio.rs`
  patched, since restored; and
  `d0c3cbd81729d01f20c772c08203c0bd4a4818b5f3d77d789cc4193104c4dfdd` after
  eleven such builds in turn, each of a patch to one of `pio.rs`,
  `toyos-userbound/src/port.rs`, `isa.rs`, `percpu.rs`, `panic_reboot.rs`,
  `idt/exceptions.rs` with `paging.rs`, and `object/ops.rs`, each restored
  before the next. Pull request #592 carries the patches and their order.
- At `b00d9a589`, built into two empty target directories by the cargo line
  `src/build.rs` runs for it: 3514808 bytes and sha256
  `cb95d0d34ac99148584083e21cbb09edc299e5a94168e8ca71a9d7ef3db8a37b` from
  each, where the kernel the worktree's own `kernel/target` had last built was
  still `2593c71c…`.

The empty directories' kernel and `2593c71c…` differ in `.symtab` and
`.strtab` and in no other section: 121 of 6419
symbol names, each in its `.llvm.<n>` suffix alone, ten of the 261 suffixes
the kernel carries. It is the suffix
`issues/build/two-checkouts-of-one-tree-build-different-guest-bytes.md`
measures across checkout paths; that issue's exit builds two clean checkouts
and would not see this one.

What moves a suffix is not measured. One of the eleven patches, `pio.rs`'s,
built in a once-empty directory and reversed, left that directory building the
empty one's kernel. `[profile.toyos]` builds with `incremental` on (root
`Cargo.toml`): the suspect, and no build with it off was compared.

What it costs: an image's signed header names the kernel file, symbol table
and all, so a reading taken on a machine names the bytes of one build, and the
next build of the same sources may carry others: a metal reading reaches a
later head by its bytes only where the builds that preceded the booted image
are replayed. `issues/build/toyos-builds-itself.md`'s M4 compares a guest
build against "the host build of the same commit", and of this kernel a host
has built three.

Exit: a target directory builds from a tree the kernel an empty target
directory builds from it, whatever the first built before. A gate reads it: it
builds a kernel into an empty directory and compares it with the one the
checkout's own target directory builds. What that checkout has built is its
"before", so no patch is its input, and on this worktree as it stands it is
red, `2593c71c…` against `cb95d0d3…`. A gate that builds an edit, reverses it
and compares is green on this defect, by the `pio.rs` measurement.
