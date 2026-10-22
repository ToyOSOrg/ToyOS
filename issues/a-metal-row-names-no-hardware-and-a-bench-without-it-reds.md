---
status: open
kind: tooling
opened: 2026-10-11
---

# A metal row names no hardware, so a bench without the T14's devices reds rows it cannot run

A `METAL` row in `tests/toyos.rs` names its boot and its judge and nothing of
the machine it needs. Rows written against the T14's devices therefore run on
any bench `--metal-host` names, and a bench without the device reds them.

Measured on the AMD bench's boot at `fb03703a1` (PR #852, the log in
issuecomment-6107031099 and issuecomment-6107031259), the `testcases` boot
that most rows ride:

- `test_rs_pci_reclaim` exited 1, `claim 1 of 8086:15fc was refused:
  NotFound`: the job claims the T14's I219, which that bench has not. The
  `claim_reuses_its_remapping_entry` row reads that job (and so did
  `claim_refused_without_remapping`, deleted by #971: without remapping a
  bench drives no USB, so its boot left no `/log`).

That run asked for `blackbox_unclaimed_page` alone, which passed. An
unfiltered `cargo test --test toyos-build -- --metal --metal-host
<the AMD bench>` would be red on those rows for the bench, not for ToyOS.
Matching a requirement against a bench rests on the machine's identity, which
the loop asks Ubuntu for today
(`issues/a-metal-run-asks-ubuntu-which-machine-it-is-and-the-boot-says-nothing.md`).

Owner: the metal harness (`tests/common/metal.rs`, the `METAL` table in
`tests/toyos.rs`); the benches are the orchestrator's.

## Exit condition

Every metal row declares the hardware it needs; a run on a bench without it
names each such row as not run and why, never counted passed; and an
unfiltered `--metal --metal-host` run on the AMD bench is red only on
defects of ToyOS.
