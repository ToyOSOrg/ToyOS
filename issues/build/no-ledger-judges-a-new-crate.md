---
status: open
kind: defect
opened: 2026-09-01
---

# A crate arriving in this tree is judged by nobody

**What `src/sourcegate.rs` closes, stated as what it closes.** Two scans landed
and each is a spelling, not a rule:

- `every_binary_the_host_runs_is_declared` refuses an undeclared argument to the
  text `Command::new(` in host Rust, with every non-literal argument pinned to a
  file and a count. Beside it, a **one-line** `use`/`type` rename of `Command`,
  after any visibility, is refused.
- `every_committed_binary_file_is_declared` refuses an undeclared committed file
  that carries a NUL in its first 8000 bytes, plus anything under `assets/`.

Everything else the two clauses asked for is an entry, not a silence:
`nothing-reads-the-workflows-for-a-binary.md`,
`the-third-party-corpus-is-in-no-machine-read-ledger.md`,
`the-one-line-alias-rule-does-not-reach-a-brace-group.md` and
`a-spawn-that-is-not-command-is-in-no-ledger.md`. A text scan over Rust source
cannot be hardened into a rule — three rounds of one-token variants said so —
and the exit named in those entries is one scan that resolves names the way the
compiler does.

**One hole is inside the gate's own table rather than in the language.** A
`Spawn::sites` row naming a file that does not exist, at count 0, satisfies both
directions of the site check and admits nothing and refuses nothing — measured
2026-09-02, green. It is one `assert` away from closed and is left here so the
next reader can spend it deliberately.

The third clause has nothing at all. *"Only general and widely used crates ---
one that does our job we write ourselves, and a driver crate never"*
(`CLAUDE.md`, "Dependencies") is a rule about arrivals, and a crate still
arrives with nobody asked.

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
transcription. This one is not in that position. Measured 2026-09-01 by the
procedure the shape above states --- the union of the `name = ` lines of every
tracked `Cargo.lock` --- **12 lockfiles and 484 unique names, 419 of them
third-party** once the 65 this repository publishes itself are removed. (The
figure carried here until now was "471 names, 43 of them direct", from a
2026-08-08 source that also said "all 28 `Cargo.lock` files"; there are 12, and
neither number reproduces.) Every one of the 419 is an acceptance nobody has
recorded.
Whether that is a decision worth making is the owner's, and it is what stands
between this entry and the same `#[test]` the other two already are.

Not reachable by any of the three, then or now: `rust/`'s own dependencies,
what a third-party build script does, the truth of a licence claim, and a fork
clone a gitignored `.cargo/config.toml` path-overrides.
