---
status: open
kind: defect
opened: 2026-10-03
---

# One checkout of one tree builds two kernels

A kernel's bytes depend on what the checkout built before it. Measured on
`wt/toyos-ps2` at `66798c7ed`, the tree clean both times: the x86-64 kernel
with `boot-actuators,test-actuators`, read back out of a staged metal image
(`toyos/kernel.elf` on slot A's volume), is 3514896 bytes and

- sha256 `2593c71c4c86f3c03815f08afc6a89eb9832c351e99376ffb4aab7858c8f83c2`
  when the build before it had one expression of
  `kernel/src/arch/x86_64/pio.rs` patched, since restored;
- sha256 `d0c3cbd81729d01f20c772c08203c0bd4a4818b5f3d77d789cc4193104c4dfdd`
  after eleven such builds in turn, each of a patch to one of `pio.rs`,
  `toyos-userbound/src/port.rs`, `isa.rs`, `percpu.rs`, `panic_reboot.rs`,
  `idt/exceptions.rs` with `paging.rs`, and `object/ops.rs`, each restored
  before the next. Pull request #592 carries the patches and their order.

The second is the kernel a T14 boot of the same sources had carried: its
loader printed the SHA-256 of the slot's signed header, and that header signed
again over the second kernel has it.

`[profile.toyos]` builds with `incremental` on (root `Cargo.toml`). That is
the suspect and is not measured: no build with it off was compared.

`issues/build/two-checkouts-of-one-tree-build-different-guest-bytes.md` is
the same defect across paths; its exit builds two clean checkouts and would
not see this one.

What it costs: a reading taken on a machine names the bytes of one build, and
the next build of the same sources may carry others, so a metal reading
reaches a later head by its bytes only where the builds that preceded the
booted image are replayed. `issues/build/toyos-builds-itself.md`'s M4 compares
a guest build against "the host build of the same commit", and of this kernel
there are two.

Exit: the two orders above give one kernel.
