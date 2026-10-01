---
status: open
kind: tooling
opened: 2026-10-01
---

# The userland gate reads an attribute only where a line starts, so a test after code on its line runs nowhere

`src/userlandhost.rs`'s `scan` reads a line for attributes only if the line
starts with one. `fn _hidden() {} #[test] fn runs_nowhere() { panic!("never run") }`
is a test to rustc and none to the gate. Appended to a package `host` never
builds, it runs in no `cargo test`, and `every_userland_test_is_in_the_gate`
stays green.

Measured with `src/userlandhost.rs` byte for byte as at 5620b737e. Each row is
a checked patch, built with `cargo run -- --build-only` (exit 0 for all),
judged with `cargo test --lib userlandhost::tests::every_userland_test_is_in_the_gate`,
and reverted:

| appended to | form | gate |
|---|---|---|
| `userland/calc/window/src/main.rs` | `#[test]` on a line of its own | 101, "a test in the nested crate calc/window" |
| `userland/calc/window/src/main.rs` | the one line above | 0 |
| `userland/snake/src/main.rs` | the one line above | 0 |

**Exit:** the scan reads an attribute wherever on a line it starts, a comment
still holds none, and the one-line form reds in both packages.
