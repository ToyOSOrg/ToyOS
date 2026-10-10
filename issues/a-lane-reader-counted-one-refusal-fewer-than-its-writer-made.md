---
status: open
kind: defect
opened: 2026-10-10
---

# A lane's reader counted one refusal fewer than its writer made

`toyos/src/log/proof.rs`'s
`a_lane_gives_its_reader_every_record_in_order_or_counts_it_refused` went red
once in `cargo run -- --ci host`'s `the toyos SDK` step, on the branch
that brings up the GICv3 ITS, which touches nothing under `toyos/`:

```
panicked at toyos/src/log/proof.rs:161:5:
assertion `left == right` failed
  left: 49999
 right: 50000
```

The reader's `Reader::refused` total was one short of the refusals the
writer thread counted from `push_lane`. The host's load average was 44.31
on 14 cores when it was read; the same step was green an hour earlier on
a tree whose `toyos/` was the same. Either the lane loses a refusal between
the writer's count and the reader's last read after `writer.is_finished()`,
or the test reads the count before the last refusal is published: which one
is not determined.

**Exit**: the cause is named, and the test reds on it without load, or the
lane's refusal count is fixed and the test holds under the load above.
