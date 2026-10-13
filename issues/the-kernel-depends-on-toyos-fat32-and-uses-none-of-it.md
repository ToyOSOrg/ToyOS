---
status: open
kind: defect
opened: 2026-10-12
---

# The kernel depends on toyos-fat32 and uses none of it

`kernel/Cargo.toml` lists `toyos-fat32` among the binary's dependencies
(`[target.'cfg(target_os = "none")'.dependencies]`), and no kernel source
names it: `git grep -n fat32 -- kernel` finds that manifest line alone, and
the kernel crate includes no file from outside `kernel/`. Its last user, the
kernel's FAT adapter, went in #536 and the edge stayed, so every kernel build
compiles a FAT32 driver it never calls. Three comments still say the kernel
uses it: `lib/fat32/Cargo.toml`'s header (the kernel depends on it by path),
`build/fatformat.rs`'s module doc ("the kernel's driver") and `populate`'s
doc in `build/image.rs` ("the driver the kernel appends `kernel.log` with").

**Exit**: `git grep -n fat32 -- kernel` finds nothing, the root `Cargo.lock`
lists no `toyos-fat32` among `kernel`'s dependencies, none of the three
comments names the kernel, and `cargo run -- --build-only` exits 0.

Owner: the orchestrator, which dispatches it.
