---
status: open
kind: defect
opened: 2026-09-01
---

# A crate arriving in this tree is judged by nobody

Two of the dependency bar's clauses are now measured: `src/sourcegate.rs` reds
on an undeclared `Command::new` argument in host code and on a committed binary
file `NOTICE` does not carry the digest of. The third is not. *"Only general and
widely used crates — one that does our job we write ourselves, and a driver
crate never"* (`CLAUDE.md`, "Dependencies") is a rule about arrivals, and a
crate still arrives with nobody asked.

The shape that was proposed on 2026-08-08 and never built: a committed file
naming every third-party crate the tree may resolve, with a one-line reason,
and a `#[test]` unioning the `name = ` lines of every `Cargo.lock` and redding,
by name and lockfile, on anything absent from it. Its whole value is that it
does not judge a crate — it forces somebody to, at the moment one arrives,
which is the step that is missing.

**The owner refused it on 2026-08-08 as brittle**, along with the other two of
that day's three, accepting only `cargo run -- --check-forks`. The other two
have since become buildable without deciding anything: `NOTICE` was written and
`CLAUDE.md` now declares the standing failures, so seeding those two ledgers is
transcription. This one is not in that position. Seeding it is 471 names, 43 of
them direct, and every one of the 471 is an acceptance nobody has recorded.
Whether that is a decision worth making is the owner's, and it is what stands
between this entry and the same `#[test]` the other two already are.

Not reachable by any of the three, then or now: `rust/`'s own dependencies,
what a third-party build script does, the truth of a licence claim, and a fork
clone a gitignored `.cargo/config.toml` path-overrides.
