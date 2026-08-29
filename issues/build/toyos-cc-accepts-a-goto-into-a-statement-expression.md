---
status: open
kind: defect
opened: 2026-08-29
---

# A `goto` into a statement expression is accepted, where both references refuse it

Found by PR #338's adversarial review, which reproduced a Cranelift verifier
stop ("uses value v5 from non-dominating inst7") on a jump into a `({ … })`
from outside it. Re-measured 2026-08-29 on both sides of the statement-
expression tail fix: on the pre-fix tree,
`int f(int c) { if (c) goto l; return ({ int i = 1; l: i + 2; }); }` and its
label-mid-block variant both die in `define_function`; on the fixed tree all
such shapes *compile silently*, because the construct's value now comes from
the tail inside the label block, which dominates its own use.

That is the wrong resting state, just quieter. Jumping into a statement
expression is a constraint clang refuses outright ("cannot jump from this
goto statement to its label — jump enters a statement expression"), gcc
documents as erroneous, and this compiler now lowers into a block where `i`
was never initialised — the read is whatever the frontend's SSA gives it,
decided by nothing in the source.

The fix is a refusal by name, the same shape `#define f(a...)` gets: a `goto`
whose label sits inside a statement expression the goto is not inside. The
labels map (`FuncCtx::labels`) is function-wide today and carries no scope, so
the refusal needs the label's statement-expression extent recorded where the
label is declared — which is the same information a correct C front end needs
anyway (C11 6.8.6.1 note: gcc's rule, since a statement expression's lifetime
model breaks on entry from outside).
