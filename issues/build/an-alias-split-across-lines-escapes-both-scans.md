---
status: open
kind: defect
opened: 2026-09-02
---

# An alias split across lines walks past both scans that refuse a one-line one

`src/sourcegate.rs`'s `no_host_file_renames_command` refuses a `use` rename and
a `type` alias of `std::process::Command`, after any visibility, **on one line**.
The scan it protects reads the text `Command::new(`, so a rename makes every row
in `HOST_SPAWNS` unreachable at once — which is why the rule exists — and the
rule is a `starts_with` on a trimmed line, which is why it is not the whole of
it.

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
that the next spelling falsifies. A rule over source text cannot say what a
name resolves to.

**Exit condition.** One scan that resolves aliases the way the compiler does, so
that `Cmd2::new("…")` and `Command::new("…")` are the same call to it. A
`syn`-parsed walk is the obvious shape and `syn` is general and widely used
(`CLAUDE.md`, "Dependencies"); a compiler-side hook is the other. Either
replaces the text scan rather than adding to it, and takes
`issues/build/a-spawn-that-is-not-command-is-in-no-ledger.md` with it.

**Two gates now, not one.**
`src/sourcegate.rs`'s `nothing_in_the_kernel_counts_a_reference_by_hand` refuses
the same shape over `kernel/src` and `toyos-sched/src`, for the names its ban
table spells — `Arc`, `mem`, `forget`, the two strong-count adjusters and the
two raw-pointer converters — because one `use alloc::sync::Arc as A;` hides
every row in that table at once. It closes the one-line `use … as …` after
visibility and nothing else.

**A brace group on one line is closed, by both scans.** Measured on this tree:

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

Until then the rules close the one-line form and the module headers and the test
docs say exactly that.
