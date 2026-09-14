---
status: open
kind: finding
opened: 2026-09-13
---

# `toyos-metal` parses its own argv instead of declaring it like the other two

`src/flags.rs` holds the vocabulary of `cargo run --` and `src/testargs.rs` the
harness's, both as `Flag` declarations read only through `Vocabulary`.
`src/bin/toyos-metal.rs` does neither: `Args::parse` (`src/metal.rs:1255`)
matches the nine flags it accepts as string patterns in `match` arms and walks
the argv by hand.

It is not the hole `src/flags.rs` closed, but it is a second way to do a thing
the tree does once: a reader takes a string rather than the declaration, and
the walk decides for itself what a missing value and a repeated flag mean.
Moving it onto `declare_flags!` and `Vocabulary` would delete the hand walk.
