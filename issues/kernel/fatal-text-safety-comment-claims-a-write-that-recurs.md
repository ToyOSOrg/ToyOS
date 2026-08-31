---
status: open
kind: defect
opened: 2026-08-31
---

# `fatal_text`'s SAFETY comment claims `SNAPSHOT` is never written twice; `refresh_capture` already writes it twice

`kernel/src/drivers/panic_console/mod.rs:512-515`:

```rust
fn fatal_text() -> View<'static> {
    if CAPTURED.load(Ordering::Relaxed) {
        // SAFETY: sound as `capture`'s write — `CAPTURED` true means `SNAPSHOT` is written and never written again, so this branch is idempotent.
        unsafe { &*SNAPSHOT.0.get() }.view()
```

`refresh_capture` (`:486-491`) is reachable only after `CAPTURED` is already
true (`if !CAPTURED.load(..) { return; }`) and, when it runs, calls `capture`
again (`:490`), which writes `SNAPSHOT` again (`:474`). So "written and never
written again" is false on the very path the comment is asserting
soundness for — `CAPTURED` true does not mean the write is done, it means
`refresh_capture`'s guard for calling `capture` a second time has been met.

Nothing about this is #345's regression: `refresh_capture` and `capture`
both stand on `main` as they are cited above, so the comment has been wrong
since `refresh_capture` was added. The soundness argument the comment
*should* make is different (single-writer discipline under `PAINTING`, or
whatever actually excludes a concurrent `fatal_text` reader from a
`refresh_capture` writer) — as written it argues from a premise the same
file's own code contradicts, in the panic path.

Exit: rewrite the comment to state what actually keeps `fatal_text`'s read
sound against `refresh_capture`'s repeat write — one comment line at the
site.

Provenance: adversarial review of PR #345.
