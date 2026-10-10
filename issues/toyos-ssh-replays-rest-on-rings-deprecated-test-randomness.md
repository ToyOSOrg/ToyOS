---
status: open
kind: tooling
opened: 2026-10-09
---

# toyos-ssh's replays rest on ring's deprecated test randomness

`toyos-ssh`'s OpenSSH replays (`tests/replay.rs`), the fuzz of bent
recordings (`tests/fuzz.rs`), the recorder (`examples/record.rs`) and the
RFC 7748 test (`tests/vectors.rs`) make `ring`'s X25519 ephemeral key
deterministic in one way: `ring::test::rand::FixedByteRandom` and
`FixedSliceRandom`.

`ring` 0.17.14 marks that module `#[deprecated(note = "Will be removed.
Internal module not intended for external use, with no promises regarding side
channels.")]` (`src/lib.rs:123-131`). `ring::rand::SecureRandom` is sealed
(`src/rand.rs:23,62`), so no type of ours can stand in. `Server<A, R>`'s `R`
exists only so these tests can pass one; the server's own randomness is
`SystemRandom`.

A `ring` release that removes the module stops every replay from building, and
with them the independent oracle of
`issues/the-ssh-server-is-toyos-own-and-russh-is-gone.md`.

**Owner**: `toyos-ssh`.

**Exit**: no source in the tree names `ring::test`, and the recordings still
replay byte for byte.
