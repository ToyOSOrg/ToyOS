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
compiles a FAT32 driver it never calls. `lib/fat32/Cargo.toml`'s header still
says the kernel depends on it by path.

**Exit**: `git grep -n fat32 -- kernel` finds nothing, the root `Cargo.lock`
lists no `toyos-fat32` among `kernel`'s dependencies, `lib/fat32/Cargo.toml`'s
header names no kernel, and `cargo run -- --build-only` exits 0.

Owner: the orchestrator, which dispatches it.
