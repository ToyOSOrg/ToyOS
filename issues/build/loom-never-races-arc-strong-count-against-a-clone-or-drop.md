---
status: open
kind: tooling
opened: 2026-09-28
---

# Loom never races `Arc::strong_count` against another thread's clone or drop

`loom`'s `rt/arc.rs` makes a clone dependent on the last `strong_count` only
and a drop on the last drop only, so a drop is never raced against a
`strong_count` and a clone only against the last one. It is the atomics'
last-access gap, which the
`[loom]` fork in `forks.toml` closes for atomics, on `loom::sync::Arc`.

**Evidence:** under stock v0.7.2 and under the fork at `d63fbd07`, this runs
one execution and sees `{2}`, where `{1, 2, 3}` are all reachable:

```rust
loom::model(move || {
    let a = loom::sync::Arc::new(0u8);
    let b = a.clone();
    let th = loom::thread::spawn(move || {
        loom::sync::Arc::strong_count(&b);
        let c = b.clone();
        drop(c);
    });
    record.lock().unwrap().insert(loom::sync::Arc::strong_count(&a));
    th.join().unwrap();
});
```

**Exit condition:** the probe reaches `{1, 2, 3}`, as a test in loom's own
`tests/arc.rs` on the fork's branch.
