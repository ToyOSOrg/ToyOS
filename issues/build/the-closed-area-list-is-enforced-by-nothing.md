---
status: open
kind: tooling
opened: 2026-09-02
---

# `issues/README.md` closes the area list and no gate reads it

`issues/README.md` names ten areas and says "That list is closed." Nothing
holds the tracker to it. `src/issuegate.rs` derives its `areas` set from the
directories under `issues/` that hold a file, so a new directory *becomes* an
area by existing, and every gate then treats citations under it as resolving.

Measured 2026-09-02 on this branch — one file under a new directory, plus a
citation of it in `NOTICE`:

```
issues/networking/an-area-nobody-declared.md
test issuegate::tests::every_citation_resolves ... ok
test result: ok. 5 passed; 0 failed; 0 ignored
```

The citation gate that refuses an unknown area was green on it, because by then
it was a known area.

The area list is closed for a reason `issues/README.md` states: an area is a
directory so that every cross-reference is a path that resolves, and the slug
rather than the position is the identity. An eleventh area nobody ruled on
splits the query surface silently — `rg -c '' issues/<area>/` stops meaning
what it meant.

**Fix.** Read the ten names out of `issues/README.md`'s Areas section and hold
the directory set against it: a directory the README does not name is a red,
and a name the README carries with no directory is a stale row. The parse is
the same shape as `Registry::read`'s in `src/redlist.rs` — read the artifact,
never restate it.

**Not fixed here.** W5 bundle 6 met it while correcting a comment in
`citation_refusals` that claimed the closed list was the authority; the comment
now says the directory is, which is true. Making the README the authority is a
gate this branch did not own.
