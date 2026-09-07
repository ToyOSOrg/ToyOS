---
status: open
kind: tooling
opened: 2026-09-07
---

# The metal track's registrations are all UNMEASURED

Thirteen names in `tests/test-durations` carry `UNMEASURED_MS`, every one of
them registered by this track and none of them ever run by CI:

```
blackbox_done_chain              blackbox_early_panic_sealed
blackbox_early_panic_sealed_muted blackbox_fault_sealed
blackbox_panic_chain             blackbox_unclaimed_page
job_deadline_reboots             loader_watchdog_arms
panic_before_peripherals_reboots panic_key_holds
panic_reboots                    screen_early_panel
screen_loader_lines
```

That is what the marker is for — `src/tiers.rs`'s `UNMEASURED_MS` is a
committed row that exists to put a new registration into one KVM measurement
run — but thirteen of them at once is the whole track's price unknown, and the
tier each belongs in with it. The track developed with no CI on the owner's
ruling that the loop is reviewed once when it works, so there was no earlier
run to price them.

**The rule, and it is the only thing owed before the pull request opens:**
nothing is priced by hand and nothing is re-tiered on a dev-host timing. The
PR's own CI measures all thirteen; the commit after it moves every name over
`FAST_COMMIT_MS` to `Tier::Nightly` with a `Why::TimerAnchored` row saying what
it guards, and `--merge-durations` writes the measured artifact over the
markers. Several are known to be timer-anchored by construction — the two chain
judges watch a guest take its own resets, `panic_key_holds` asserts that nothing
happened for a span of host clock — so those rows are expected, not a surprise
to be argued with when they come.

A fourteenth, `screen_boot_bands`, was deleted with `toyos-bootband` rather than
priced.
