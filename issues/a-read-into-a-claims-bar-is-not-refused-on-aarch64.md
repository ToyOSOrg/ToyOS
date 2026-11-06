---
status: open
kind: defect
opened: 2026-11-06
---

# A read into a claim's BAR is not refused on AArch64

`virt_claim_lpi`'s `claim_bar_fault` job (`tests/guest/rust-tests/src/bin/claim_bar_fault.rs`)
reads a pipe into a slice that points at its claim's mapped BAR registers and
expects `Err(BadAddress)`. On main at 0b2dec674 the read answered `Ok(16)`,
and the job ended at line 100: "a read into the BAR was not refused
BadAddress", left `Ok(16)`, right `Err(BadAddress)`.

Seen on the guest runner's aarch64 row, both at main's own commit (run
42124293522, branch `storm-main-check`) and at #1193's rebased head (run
42120673220); the same row passed at #1193's heads before the rebase, on
#1188's base (runs 42035960983, 42077063953). Between the two bases main
took, among others, #1150, which routes every user copy through the process's
own mapping. Which change made the read succeed, and whether the kernel or
the test's expectation is now wrong, is not established.

Evidence: https://github.com/ToyOSOrg/evidence/tree/05c9ce10c698b6618a8ac9c5f38f41fe1e2451a0/pr-1193/r4/logs

Owner: the memory track (S0-a). Exit: a read whose destination is a claim's
BAR is refused as the kernel's copy contract states, or the contract and the
test say otherwise, and `virt_claim_lpi` passes on main.
