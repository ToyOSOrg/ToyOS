---
status: open
kind: tooling
opened: 2026-10-02
---

# A compile-fail case names an error code rustdoc never reads

rustdoc reads the error code of a `compile_fail` block only on a nightly build
(`ErrorCodes::from(…is_nightly_build())`, `rust/src/librustdoc/doctest.rs`),
and the host suites run the host's stable one, 1.98.1 where this was measured:
the `toyos` toolchain builds no rustdoc. There the code is a word of the fence,
and the block passes on any compile error, a renamed item or a typo included.

Each code changed to `E0308`, a checked patch run and restored, leaves its
crate's doc-tests green:

| case | names | `cargo test -p <crate> --doc` under `E0308` |
|---|---|---|
| `toyos_bootmap::DirectMapEnd` | `E0603` | EXIT=0 |
| `toyos_transport::Place`, three blocks | `E0080` | EXIT=0 |

What does hold a case to its reason in this tree is the block beside it that
compiles (`toyos-net-wire/src/lib.rs`, `mod compile_fail`). `Place`'s three
have one; `DirectMapEnd`'s has none. The "compile-fail case" that Stages C and
H of `issues/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`
exit on is held the same way and no other.

**Exit**: `git grep 'compile_fail,E'` finds nothing outside `rust/` and every
compile-fail case sits beside a block that compiles; or the host suites'
rustdoc reads the code, and a case under a wrong one reds its crate's
doc-tests.

Owner: the orchestrator, which dispatches the stages whose exits name a
compile-fail case.
