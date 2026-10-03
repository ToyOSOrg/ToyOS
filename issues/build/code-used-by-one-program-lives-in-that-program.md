---
status: open
kind: track
opened: 2026-09-29
---

# Code used by one program lives in that program

A crate exists because two programs share it. What only the kernel uses is the
kernel package's, what only one userland program uses is that program's, and
shared crates with one subject are one crate (owner, 2026-09-29). No gate holds
the layout: `.claude/agents/reviewer.md`'s Fit line states it.

An input boundary is a crate of its own, whoever uses it: the no-panic
track (`issues/kernel/a-panic-is-never-an-accident.md`) forbids its tier 1 per
crate, and a crate that holds a tier-2 stop cannot forbid the set. A crate is
one when its own source decodes a word from outside its trust, or bounds it by
its form: hardware registers, firmware tables, disk bytes, network bytes, or
what another program sent, a syscall's arguments included. A lookup of a key a
program named is neither. No step here merges one; each stays a crate under
the no-panic track.

Every step lands green on `--ci host` and `--build-only`. Test counts are what
`cargo test -p <package> -- --list` lists today. Step 4 lands before the
latency work (`issues/kernel/toyos-beats-linuxs-latency-on-the-t14.md`): asked
whether all five steps land in the window right after #592, the owner chose
"All five steps — The whole consolidation right after #592 lands, one step per
change, before the speed work starts" (2026-10-03).

4. **A crate one package uses goes under it, a crate of its own.** A crate of
   this tree with exactly one consumer moves under it. Its consumers are the
   packages that name it as a dependency of any kind, under any `cfg`, as
   `cargo metadata --no-deps` reads every manifest `git ls-files '*Cargo.toml'`
   lists, excluded packages and `tests/` included; a crate the images ship as a
   program of its own counts as its own consumer. Of two crates that are each
   other's only consumer, the one the other names only as a dev-dependency
   moves under it, and the count treats the pair as one crate. A move under a
   userland program lands with `src/userlandhost.rs`'s survey gating a nested
   crate's tests, which it lists as escapes today.
   Check: `--ci host` runs every test each package lists today, and `--clippy`
   lints them.

**Exit:** no directory this file names as moved or merged still exists, and
step 4's count finds no crate outside its one consumer.
