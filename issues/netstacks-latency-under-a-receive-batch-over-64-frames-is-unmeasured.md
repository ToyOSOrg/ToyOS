---
status: open
kind: tooling
opened: 2026-10-12
---

# netstack's latency under a receive batch of more than 64 frames is unmeasured

`Node::receive` takes at most the link's receive ring in one batch, 256
frames whatever the driver (`toyos_link::layout::RX_DEPTH`,
`system/netstack/src/link.rs`). Before the drivers left netstack it took the
card's ring, 999 frames on the I219, and before pull request #861, 64. Every
frame of a batch reaches the stack under one `now`, read before the batch
(`system/netstack/src/main.rs`'s loop), and the deadlines that are due, the
transmit opportunity and the clients' answers run only after the batch's last
frame. So a backlog that was served as several 64-frame passes, with those
three between them, is now one pass: a frame late in it is stamped with the
batch's start, and a deadline due during it fires after it.

Nothing moves while a pass brings 64 frames or fewer, and nothing has read a
pass that brings more. The T14's LAN round trip was read only under one
gigabit download of full-sized frames, at about 12 frames a pass (round 4 of
pull request #861: 59.7 passes per MB, and some 700 full-sized frames carry a
MB; derived). A pass brings more than 64 when netstack is about 0.79 ms behind
full-sized frames at a gigabit (64 frames of 12.3 µs on the wire, derived), or
under minimum-sized frames, of which a gigabit carries about 195 in one
131 µs throttle interval (672 ns a frame on the wire, derived).

**Exit**: a T14 reading, in one session, of the LAN round trip and of how late
netstack fires a due deadline, under a load that brings more than 64 frames a
pass, with the ring's budget and with a budget of 64, recorded in the commit
that closes this. A gap between the two is investigated where it shows, and is
no verdict on the budget by itself.

**Owner**: whoever holds `issues/the-lan-is-not-yet-production-grade.md`, whose
stage 2 sets the ring sizes and the interrupt moderation this batch follows.
