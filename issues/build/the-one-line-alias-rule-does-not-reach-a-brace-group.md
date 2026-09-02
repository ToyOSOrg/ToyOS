---
status: open
kind: defect
opened: 2026-09-02
---

# A `Command` alias spelled over two lines walks past the rule that refuses one

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

Until then the rule closes the one-line form and the module header and the test
doc say exactly that.
