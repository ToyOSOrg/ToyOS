---
status: open
kind: defect
opened: 2026-09-27
---

# `home_budget_refusal_retried` is red on every nightly

`home_budget_refusal_retried` (nightly tier) is red, and red alone, on the
nightlies of three trees, each time with the same two shapes:
- main at e8d7c9c0 (run 36228604597);
- PR #524's branch at 8c5be843 (run 36273557690);
- main at fd62f567 (run 36278449733, guest (12)).

The two shapes: `no fsync: /home/... durable on attempt line — the retry
never ran`, and `Boot timed out waiting for ===READY===`. In the second, the
console ends at the loader's `Applied 5483 relocations`, with
`fsync-budget-spent` on the boot parameter line. It was green at 3f46a019
(run 36111884575).

It is green alone on a dev host at f231c43e (`cargo test --test toyos-build
-- --nightly home_budget_refusal_retried` EXIT=0).
`cargo run -- --known-red home_budget_refusal_retried` answers NO.

`fsync-budget-spent` is the machine-wide actuator that
issues/boot-media/partition-claim-gives-up-reds-beside-other-guests-and-is-green-alone.md
names as racing whichever fsync the boot reaches first. Not shown: whether
that race is this test's cause.

**Exit**: the cause shown on a red run's log.
