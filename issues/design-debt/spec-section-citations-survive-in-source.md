---
status: open
kind: defect
opened: 2026-08-25
---

# `§N.N` citations in source point into a spec corpus the tree deleted

The spec documents were deleted by owner ruling; the deleting merge did not
sweep the source comments that cite their section numbers, so `§2.3a`, `§6.4`,
`§16.1` and kin survive across kernel files as pointers at nothing — the exact
rot the tracker's deletion law exists to prevent, and no gate catches it
because the marks live in prose.

The prose sweep's third wave replaced every marker in its thirteen files with
the contract the section actually stated; the first two waves, working
concurrently, left theirs standing rather than resolve inconsistently.
`rg -n '§[0-9]' kernel/src` is the finder. The fix per site is wave three's:
say what the section said in one clause, or delete the sentence that leaned on
it — never a bare unmarking, which keeps the sentence and loses its ground.
