---
status: open
kind: tooling
opened: 2026-10-09
---

# The licence step re-locks the std workspace against the registry on every run, so `host` reds without the network and judges whatever the registry serves that day

`licence::judge` (`src/licence.rs`) copies the fork's `library/Cargo.lock`
into `target/licence/` and runs `cargo metadata` over the std workspace with
`resolver.lockfile-path` pointing at the copy, with neither `--locked` nor
`--offline`. The copy is stale by design, since bootstrap re-locks it to this
tree's `toyos-abi` and `toyos` versions, so cargo resolves it again and asks
the registry each time. Two things follow. `cargo run -- --ci host` cannot
pass on a machine without the network, though nothing else in it needs one.
And the set of crates whose licences the step judges is not pinned by anything
in the tree: it moves with what the registry answers on the day of the run.

Evidence: during a network outage on the development machine, the `--ci host`
runs of two branches were each red in this one step, `the licences of what
ships`, 77 of 78 steps green, the step's `cargo metadata` failing to resolve
the registry's host name after its retries; both were green on the same heads
once the registry answered again. Neither branch touched anything the step
reads.

**Exit**: `the licences of what ships` passes with the network off, shown by a
check that runs it so, and what it resolves for the std workspace is a
function of the tree alone. Owner: the build system (`src/licence.rs`).
