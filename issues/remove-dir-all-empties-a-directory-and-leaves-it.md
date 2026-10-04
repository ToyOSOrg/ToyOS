---
status: open
kind: defect
opened: 2026-09-05
---

# `std::fs::remove_dir_all` empties a directory and never removes it

`rust/library/std/src/sys/fs/toyos.rs`'s `remove_dir_all` walks the directory,
unlinks every file and recurses into every subdirectory, and returns `Ok(())`
without ever calling `rmdir` on anything — its own path included. Every
directory in the tree it walked survives, empty.

`SYS_RMDIR` (54) exists and `rmdir` in the same file wraps it; nothing calls it
on this path.

## Reproduction

Measured in `pkg_install_gbae`'s guest on 2026-09-05, before `userland/pkg`
stopped using it. `pkg remove gbae` reported success and `pkg list` was empty —
`/apps/gbae/manifest.toml` was gone, so init refused a launch out of it — and
the next `pkg install` of the same archive answered

    pkg: /apps/gbae is already there — remove it first, this installer does not replace

The kernel's own syscall census for that process is the shorter proof: four
`SYS_DELETE` and no `SYS_RMDIR` at all.

    [kernel 1.004 cpu1] syscalls: pid=14 total=18 syscall_wall=11ms 0=1 1=2 6=1
    9=1 10=1 14=1 17=1 18=4 63=2 72=1 73=2 91=1

`/system/bin/rm -r` is the other caller (`userland/toybox/src/rm.rs:30`) and has
the same hole.

## Exit condition

`remove_dir_all` removes the directory it was given and every directory under
it. Closes when that lands in the fork and `userland/pkg`'s own `remove_tree`
is deleted with it — that walk exists only because this one does not finish.

The fix is one `rmdir(path)` after the loop, and it is in the sysroot's fourth
source (`rust/library`), so it lands on its own branch with the machine's
sysroot claim.
