---
status: open
kind: defect
opened: 2026-09-27
---

# Netstack3's reset of a connection probing a shut window lands past it

When the core aborts a connection whose peer holds its window at zero, the RST
it sends carries SND.NXT, and SND.NXT is one past the byte its last
zero-window probe sent. The peer's RCV.NXT is still that byte, so the RST's
sequence number is outside a zero window and a peer following RFC 9293
§3.10.7.4 drops it without a word: it stays ESTABLISHED until it next sends
something, which this stack then answers with a reset of its own. Linux sends
such a reset at the window's right edge (`tcp_acceptable_seq`), which a shut
window's receiver accepts.

**Seen** in `userland/netd/src/stream/tests.rs`,
`an_orphan_the_closing_table_has_no_room_for_is_reset_and_gone_at_once`, two
cores on one wire: the probes `seq=835278557 len=1` answered
`ack=835278557 win=0`, then `RST|ACK seq=835278558`, and the peer stayed
ESTABLISHED through 30 more seconds of passes. netd reaches it through
`stream::abort` (`disconnect_bound`), on a stream it gives up.

The code is in the mirror (`fuchsia/upstream/`, `netstack3-tcp`'s `abort`
path), which this repository never edits.

**What would close it**: the fix upstream, synced in, and the test's
`the premise: the first RST was dropped` assertion turned into the reset
arriving at once.
