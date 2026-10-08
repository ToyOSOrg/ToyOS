---
status: open
kind: defect
opened: 2026-10-08
---

# `toyos-elide`'s `limit` has one user and lives in a shared crate

`toyos-elide/src/limit.rs` (`Limit`, `Admit`) is used by
`userland/logkeeper/src/origin.rs` and by nothing else: the kernel's
`log_limited!`, its other user, is deleted with the thread-exit record it
limited. The crate is shared for `Elided`, which `toyos-symbols` uses; what
one program alone uses belongs in that program's package
(`.claude/agents/reviewer.md`, "Fit").

Not moved where it was found: PR #773 is open over `userland/logkeeper/src`,
and the module moves into the tree that leaves.

Owner: `userland/logkeeper`.

## Exit condition

Once #773 has landed: `limit.rs` is a module of `userland/logkeeper` with its
host tests, `toyos-elide` holds `Elided` and what serves it, its header and
`description` say so, and `rg 'toyos_elide::limit'` finds nothing.
