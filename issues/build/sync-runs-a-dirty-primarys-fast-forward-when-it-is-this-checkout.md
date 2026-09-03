---
status: open
kind: defect
opened: 2026-09-03
---

# `--pr`'s sync still skips the *dirty* primary question when the primary is this checkout

`src/pr.rs`'s `sync` asks two things before it fast-forwards the primary: which
branch it has out, and whether it is dirty. PR #382 moved the first out of the
`canonical(root) != canonical(&primary)` guard and left the second inside it:

```
    if canonical(root) != canonical(&primary) {
        let dirty = git(&primary, &["status", "--porcelain"])?;
```

So on a primary that is the checkout running `--pr`, a dirty tree still reaches
`git -C <primary> merge --ff-only origin/main`, git refuses it, and `sync`
answers with `stranded()` — commits `origin/main` has not got, and a
`reset --hard` — about a `main` that is a plain ancestor. The same false report
as the branch case, one case over. `prepare` refuses its own dirty tree first,
so what reaches here is a dirtiness it does not refuse.

Judge: `src/pr.rs`'s `a_primary_on_a_branch_is_left_where_it_is` is the shape —
a fixture clone is its own primary. A sibling that leaves it on `main` with an
untracked file reds on today's tree with the `stranded()` text.

Exit: the dirty question is asked before the fast-forward, whether or not the
primary is this checkout, and that sibling is green.
