---
status: open
kind: finding
opened: 2026-09-13
---

# `toyos-metal` parses its own argv instead of declaring it like the other two

`src/flags.rs` holds the vocabulary of `cargo run --` and `src/testargs.rs` the
harness's, both as `Flag` declarations read only through `Vocabulary`. The third
command line in this tree, `src/bin/toyos-metal.rs`, does neither:
`Args::parse` (`src/metal.rs:1274-1330`) matches the nine flags it accepts as
string patterns in `match` arms and builds its own usage text.

It is not the hole `src/flags.rs` closed — an unknown word there is already
`Refusal::Usage` (`src/metal.rs:2286`) — but it is a second way to do a thing
the tree does once: its flags are spelled twice (the arm and the usage text),
a reader takes a string rather than the declaration, and nothing holds the two
against each other. Moving it onto `declare_flags!` and `Vocabulary` would
delete the hand walk and the usage text together.
