---
status: open
kind: tooling
opened: 2026-09-06
---

# Two sequences build a boot image, and `Boot::case` refuses by name to keep each artifact to one writer

`build::build` and `build::build_test_image` each assemble a kernel, a
bootloader and a ROOT image into a disk. They differ in what they memoize
(`build_test_image` keys the three parts and reuses them across a suite run;
`build` rebuilds), in whether they take `extra_files`, and in whether they
create `target/nvme.img`. `--boot-config` takes the second, the three modes
take the first, and `Boot` carries a `case` flag to say which.

While both exist, an image with two possible writers is an image whose contents
depend on which command last ran, so `Boot::case` refuses the three mode
directories by name and the flag cannot reach `target/bootable.img`,
`target/bootable-diag.img` or `target/bootable-console.img`.

**Exit condition**: one function builds every image, `Boot::case` and the
`case` flag go, and `--boot-config .` builds the shipped config like any other.
What has to be answered first is whether the memo is safe on the `cargo run`
path — it is keyed per process, and a `cargo run` builds one image, so the
question is only whether `create_sparse` for `nvme.img` belongs to the caller.
