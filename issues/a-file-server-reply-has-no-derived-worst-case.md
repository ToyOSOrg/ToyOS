---
status: open
kind: defect
opened: 2026-11-12
severity: [hang]
---

# A file server's reply has no derived worst case

A file server's client lets its server go once it has waited `REPLY_BOUND_NS` for a reply it is owed: the block
protocol's silence bound, 365 s, and 60 s of room (`sdk/toyos/src/fs.rs`). Every request in flight on that connection
then fails `Unanswered`, and every file it opened answers `Stale`.

Nothing derives what a reply can take. One request may wait on several block requests in turn, each allowed the
block protocol's silence: an fsync writes back every page its file owes inside the server's one loop
(`system/fileserver/src/pages.rs`), and a write that needs room writes back first. The server answers its other
connections one request each in between, and nothing bounds how many there are. So a server that keeps its block
contract can stay silent past the bound and be let go, and the bound itself rests on no figure of the server's.

**Exit**: a derived worst case for a file server's reply, from the block requests one request may wait on and the
turns it may wait behind, with the client's bound set from it; or a server that tells a client it owes a reply is
still being worked on, so that silence past a bound means a hang.

**Owner**: agents/fileops (the file server and its client).
