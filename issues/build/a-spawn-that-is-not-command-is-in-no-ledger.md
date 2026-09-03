---
status: open
kind: tooling
opened: 2026-09-02
---

# A text scan over Rust source cannot say what a name resolves to, so neither the spawn ledger nor the alias rules that guard it are the bar

`src/sourcegate.rs`'s `every_binary_the_host_runs_is_declared` reads every
`Command::new` argument in host Rust against a declared table. `Command` is not
how a host binary has to be started, and the rules that stop the scan being
renamed away are text rules with the same limit.

## The spawn that is not `Command`

Measured 2026-09-02, planted in `src/ci.rs` and green with `218 passed`:

```rust
unsafe { libc::system(c"/usr/bin/curl --version".as_ptr()) }
```

`libc` is a **direct** dependency of `toyos-build` — `Cargo.toml` names it for
`statvfs`, `getloadavg` and the libproc pair — so this compiles today with
nothing added. A whole shell command line runs on the host and no gate sees a
binary at all. `libc::execvp`, `libc::posix_spawn` and a `sh -c` string handed
to any of them are the same hole under other names.

**Why the scan cannot simply grow a row for each.** `libc::system` takes a
`*const c_char`, so the name is not in the call at all — `c"…"` here, a
`CString` built three lines up in the next case. That is the same wall the
non-literal `Command::new` arguments hit, and there the answer was to pin them
to a file and a count; there is no equivalent when the argument is a pointer
into a buffer.

**The narrow thing that is worth doing on its own** is a ban rather than a
ledger: `libc::system`, `libc::execvp`, `libc::execve`, `libc::posix_spawn` and
`libc::fork` have no legitimate caller in this repository today, and a
`src/sourcegate.rs` `Ban` with an empty `allowed` list says so in the shape that
file already uses. That still leaves a hand-rolled `syscall!` and every crate
that spawns for us.

## The alias that no one line spells

`no_host_file_renames_command` refuses a `use` rename and a `type` alias of
`std::process::Command`, after any visibility, **on one line**. The scan it
protects reads the text `Command::new(`, so a rename makes every row in
`HOST_SPAWNS` unreachable at once — which is why the rule exists — and the rule
is a `starts_with` on a trimmed line, which is why it is not the whole of it.

Measured 2026-09-02, planted in `src/ci.rs` and green with `218 passed`:

```rust
use std::process::{
    Command as Cmd2,
    Stdio as Stdio2,
};
```

The line carrying the rename does not begin an item, so nothing looks at it.
Nothing in this tree would collapse it either: there is no `rustfmt` gate and no
`rustfmt.toml`, so the shape survives a landing.

**Not fixed by a third pattern, on purpose.** Round one refused
`use … Command as`; round two's mutation was `pub use …`, closed by stripping
visibility; this one is the same move again. Every one of those fixes is fitted
to the string that defeated the last, and each carries a claim about "the item"
that the next spelling falsifies.

**Two gates carry this shape, not one.**
`nothing_in_the_kernel_counts_a_reference_by_hand` refuses it over `kernel/src`
and `toyos-sched/src`, for the names its ban table spells — `Arc`, `mem`,
`forget`, the two strong-count adjusters and the two raw-pointer converters —
because one `use alloc::sync::Arc as A;` hides every row in that table at once.
It closes the one-line `use … as …` after visibility and nothing else.

**A brace group on one line is closed, by both.** Measured on this tree:

```
use_renames("use alloc::sync::{Arc as C, Weak};", "Arc")            = true
use_renames("pub(crate) use alloc::sync::{Weak, Arc as C};", "Arc") = true
renames_command("use std::process::{Command as Cmd, Stdio};")       = true
renames_command("pub use std::process::{Stdio, Command as Cmd};")   = true
```

What escapes is a `use` **split across lines** — the block quoted above, whose
rename does not begin an item on its own line. Beside it: a plain re-import
(`use core::mem::forget;` then `forget(x)`) and a `type` alias. The `type` half
is deliberately absent from the kernel scan — `type PageTables = Arc<…>` is
ordinary and `kernel/src/process.rs:43` writes one — so
`PageTables::increment_strong_count` is a spelling neither gate reads.

## Exit condition

One scan that resolves names the way the compiler does, so that `Cmd2::new("…")`
and `Command::new("…")` are the same call to it, over the whole set of spawn
entry points rather than one spelling of one of them. A `syn`-parsed walk is the
obvious shape and `syn` is general and widely used (`CLAUDE.md`,
"Dependencies"); a compiler-side hook is the other. Either replaces the text
scans rather than adding to them. Until then the rules close the one-line form
and the module headers and the test docs say exactly that.
