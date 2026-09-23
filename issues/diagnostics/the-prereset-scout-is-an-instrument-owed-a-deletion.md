---
status: open
kind: tooling
opened: 2026-09-23
---

# `toyos-i219`'s pre-reset scout is a bench instrument, and it is owed a deletion once its reading is taken

`toyos-i219/src/scout.rs` is not a driver. netd's `--exit-with-crumbs` arm
runs it on the I219 ahead of `I219::open_trailing`: it asks the PHY for its
identifier on the part as the firmware handed it over, resets the MAC alone as
the bring-up did up to run 111, waits §9.2's 10 ms, and asks again — every
transaction's `MDIC` word a durable `saw MDIC` crumb behind a `Step::Ask`. The
trail then says in the part's own words whether that reset is what took the
PHY out of `MDIC`'s reach on the T14.

**What it costs.** Two holds of §8.2.4's flag, one extra MAC reset and one
pace on every crumbs boot of the T14, and a reset of the MAC alone that the
bring-up after it has to recover from — the ladder in `phy::wake` is what
recovers it, so on the boot it runs on, the bring-up is measured from a worse
start than a shipping boot's.

## Owner

The I219 bring-up's author, and after it the network track.

## What would close it

One T14 trail with both asks on it. Then the module goes, with
`Whole::MacAlone`'s use from it, `Moment::BeforeReset` and
`Moment::AfterReset`, the call in `userland/netd/src/i219.rs`'s
`leave_crumbs`, and `the_scout_reads_whether_a_mac_alone_reset_loses_the_phy`
in `toyos-i219/src/tests.rs`. It goes no later than the crumb arm itself.
