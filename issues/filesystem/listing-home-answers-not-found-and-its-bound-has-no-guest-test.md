---
status: open
kind: defect
opened: 2026-09-30
---

# Listing `/home` itself answers "entity not found", and the listing bound on a served directory has no guest test

`readdir_bound` asserted that `read_dir("/home")` is refused once `/home` holds
more entries than the listing bound, and it accepted any error. Before the file
servers the refusal was the bound's; since them it is `NotFound`, whatever the
count:

```
634-whole.log (main before #536):   [readdir]: /home refused at 32769 entries (out of memory)
536r21-whole.log (#536's branch):   [readdir]: /home refused at 32769 entries (entity not found)
536r21-whole.log:                   [readdir]: /home's own root is still refused (entity not found)
```

So on `main` the arm created 32,769 files through the DATA server (364 s and
442 s of `readdir_bound` on #536's two whole runs, against 46-181 s before it)
to reach an answer an empty `/home` gives too. The arm is deleted; what is left
untested is the one refusal the DATA server has for a listing too large, the
`WINDOW_BYTES` check in `userland/fsd/src/main.rs`'s `READDIR`.

## Owner

The storage track, which moved `/home` behind the DATA server.

## What would close it

`read_dir("/home")` on a machine with a DATA volume answers `/home`'s entries,
and a guest test lists a served directory past `WINDOW_BYTES` and is refused
with `ResourceExhausted` by name rather than by any error.
