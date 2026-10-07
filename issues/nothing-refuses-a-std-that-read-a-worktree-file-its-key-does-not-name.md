---
status: open
kind: tooling
opened: 2026-10-07
---

# Nothing refuses a std that read a worktree file its key does not name

A sysroot's key reads the trees `src/sysroot.rs`'s `SYSROOT_SOURCES` lists,
and nothing holds that list against what std's build read. With `sdk/std`
taken off it, an edit to `sdk/std/sys/pal/mod.rs` that makes every program
exit one higher left the key at `46c572c6e5176094`: `cargo run --
--build-only --arch aarch64` finished in 2 s having built no std, and
`virt_readonly_copyout` passed on the std built before the edit.

`toolchain::assert_std_built_from` reads std's dep-info and decides only
whether its `toyos-abi` and `toyos` sources are this worktree's.
`assert_std_reads_no_worktree` holds the freestanding libraries to reading
nothing of the worktree; ToyOS's std has no check of what it may read there.

**Exit:** a build of ToyOS's std whose dep-info names a file of the worktree
that is outside its fork and outside every tree the key reads is refused by
name, and a test builds that case.
