---
status: open
kind: tooling
opened: 2026-09-04
---

# A branch holding the sysroot cannot measure the base it is compared against

A branch whose `toyos-abi/src` differs from main's claims the shared sysroot on
its first build and holds it until it lands (`src/toolchain.rs`). Every other
checkout is then refused, including this one with the base checked out:

    this worktree and the shared sysroot at /Users/jan/Dev/jan/toyos/rust
    disagree about toyos-abi/src ... Your toyos-abi and toyos are
    byte-identical to main's, so there is nothing here to claim with ...
    Do not pass --claim-sysroot.

That is correct as a lock and wrong as a measuring rule: the two-check law asks
every high-risk change for an arm measured on the base, and a boot-time or
audio comparison needs a base *guest*, which needs a kernel, which needs the
sysroot. `rootfs-root-partition` measured its own boot-to-ready and could not
measure main's, so its pull request carries one arm and a declared gap.

**Reproduction.** Claim the sysroot from an ABI-bearing branch, check the base
out in the same worktree, run `cargo test`. The refusal above, before any guest
boots.

**Exit condition.** An ABI-bearing branch can take a guest measurement on its
own base without releasing the claim — a second sysroot keyed by the sources it
was built from, or a measurement taken before the first claim and recorded with
the branch.
