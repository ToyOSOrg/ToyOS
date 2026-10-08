---
status: open
kind: defect
opened: 2026-10-02
---

# A close of one handle ends every ring's poll on its object

`ops::close` answers every poll on a watch its object ends with `-NotFound`
(`Watch::cancel_polls`), in every ring, when any one handle to a pipe's read
end or an acceptor closes. A sibling handle from `dup` keeps the object open,
and its polls end all the same. `toyos::poller`'s `wait` hands the token of a
negative completion to its caller as it does a ready one's and drops the
kernel's word for it, so a reader that takes that token for bytes and reads
blocking parks on an object that is open and empty. `wait_answers` hands the
word beside the token. No caller in the tree reaches it: fsd's acceptors are endowed and
never duplicated. `inbox_cancel_wakes` stages the close.

**Exit**: a poll ends only when the last handle to its source closes, or
`wait`, the form that drops a completion's result, is gone and every caller of
`Poller` reads it through `wait_answers`; a test closes a duplicate under a
watch and reads what the wait hands back.
