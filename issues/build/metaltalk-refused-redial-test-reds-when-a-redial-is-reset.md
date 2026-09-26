---
status: open
kind: defect
opened: 2026-09-26
---

# metaltalk's refused-redial test reds when a redial is reset, not refused

`metaltalk::tests::a_redial_counts_every_refusal_and_gives_up_at_its_ceiling`
(`src/metaltalk.rs`) failed once in `cargo run -- --ci host`'s
`cargo test --lib`, with the 1-minute load average between 17 and 43 from
work outside the run. Its `assert!(why.contains("ceiling") &&
why.contains("refused"))` read:

```
127.0.0.1:64160 cannot be reached from this host: Connection reset by peer (os error 54)
```

The test drops its loopback listener and expects every redial to be refused.
One was answered `ECONNRESET` instead, which the dial loop does not count
toward the ceiling: it gives up at once with "cannot be reached". Run alone
twenty times on the same tree right after, it was green twenty times. The
branch it reddened on (#508) touches nothing under `src/`.

Exit condition: a reset dial to a dropped loopback listener is either counted
as the refusal it is or shown not to occur, measured under the same load.
