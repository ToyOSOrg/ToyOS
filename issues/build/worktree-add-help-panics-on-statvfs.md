---
status: open
kind: defect
opened: 2026-08-31
---

# `cargo run -- --worktree add --help` panics instead of printing help or refusing the flag

Run 2026-08-31 from a checkout root:

```
$ cargo run -- --worktree add --help
...
thread 'main' (64138958) panicked at src/worktree.rs:301:5:
statvfs : No such file or directory (os error 2)
```

`src/worktree.rs:32-40` finds `add` after `--worktree` and takes the very
next argument as the path operand with no validation:

```rust
Some("add") => add(root, &operand.expect("--worktree add needs a path")),
```

So `--help` is consumed as the worktree's target path. `add` (`:74`) calls
`free_bytes(path.parent().unwrap_or(Path::new("/")))` on that path, and
`free_bytes` (`:293-302`) calls `libc::statvfs` on it, asserting the return
is zero (`:301`). `statvfs` on the parent of a bare relative path like
`--help` fails (`ENOENT`), and the `assert!` panics rather than the caller
refusing the unrecognized flag by name.

Exit: `--worktree add` validates its operand isn't itself a flag (or the
top-level parser recognizes `--help` before reaching subcommand dispatch)
and refuses with a message naming the bad argument, rather than reaching
`statvfs` on a path nobody chose.

Provenance: found operating the build tool while filing wave-4 review
findings (this pass).
