---
status: open
kind: tooling
opened: 2026-09-25
---

# `lan_talk_host_closes` judges a fixed ten-second window

`tests/common/lan.rs`'s `lan_talk_host_closes` boots the e1000e talk image,
drains the serial console for a flat 10 s (`drain_serial(Duration::from_secs(10))`),
and then counts the host listener's accepts. If the boot has not brought the
82574 up, leased, and opened its log stream inside those 10 s of host wall
clock, the count is 0 and the test fails with "the boot never connected, so
nothing here was closed on it".

Seen at `c22e326f` (`wt/toyos-inspect`) in a fast-tier run beside another
worktree's suite, with all 12 guest slots held by two suites:
`FAIL lan_talk_host_closes (15s)`, then `ALONE lan_talk_host_closes: GREEN`.

The window paces the verdict and does not wait for an event. It should wait on
the listener's first accept, bounded by a ceiling that fails loudly, and only
then pace the offers into the closed connection.

**Exit condition**: the test waits for the accept event instead of for a fixed
span, and stays green under a loaded fast tier.
