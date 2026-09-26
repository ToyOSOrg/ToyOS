---
status: open
kind: tooling
opened: 2026-09-26
---

# The toolchain pins an older commit of three forks userland consumes

`forks.toml` keeps one branch per upstream base, with every fix appended to
it. Three bases break that: getrandom 0.2 and 0.3, and libloading. The rust
fork's `rust/Cargo.lock` pins `toyos-0.2`, `toyos-0.3` and libloading's
`toyos` at the commit before `toyos-abi` moved to 0.12, while `userland/` and
`tests/toyos-rust-tests` consume `toyos-0.2-sdk-0.12`, `toyos-0.3-sdk-0.12`
and `toyos-sdk-0.12`: the same branch plus the SDK range and, for getrandom, a
refused `SYS_RANDOM` answered as an error. Appending those commits to the
base-named branches would leave `rust/Cargo.lock` behind them, which
`cargo run -- --check-forks` reports as a drift, so each base has two branches
until the toolchain moves.

**Exit**: `rust/Cargo.lock` re-locked onto the `-sdk-0.12` heads, the three
base-named branches fast-forwarded to them, and `userland/` and
`tests/toyos-rust-tests` consuming the base-named branches again.
