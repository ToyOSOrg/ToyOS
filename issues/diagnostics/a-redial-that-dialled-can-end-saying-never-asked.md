---
status: open
kind: tooling
opened: 2026-09-28
---

# A redial that dialled can end saying it never asked

`src/metaltalk.rs`'s `serve` asks a redial's connection closed before a line
again while `Instant::now() < until`, and `open` then reads the clock afresh
before its first dial, starting from `last = "never asked"`. When the bound
passes between those two reads, the redial's reason (`Stream::unopened`) is
`… was not serving its log by the bound: never asked`, although it dialled and
counted every close (`Stream::turned_away`). The metal loop records that
reason as the redial's end. Read from the code, not yet observed.

`metaltalk::tests::a_redial_ends_at_its_bound_alone_and_says_so` holds each
connection past the bound so its redial ends on `serve`'s check, and does not
reach this window.

## Exit condition

A redial whose bound passes after it has dialled names what its latest dial
got, and a test that stages the bound passing between the two reads shows
that it does.
