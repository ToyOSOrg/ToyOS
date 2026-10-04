---
status: open
kind: defect
opened: 2026-09-28
---

# The T14 redial re-asks mDNS after every refusal

`open` (`src/metaltalk.rs:383`, `:419`, `:452`) sets `on_the_link` on the
first failed dial and never clears it, so once the old or new netd starts
sending refusals or resets, every dial that follows first asks the link for
the name again (`ask_the_link`) before it redials. Without a dial ceiling to
stop that loop, a gap where the machine keeps answering with a refusal sends
multicast queries at LAN round-trip rate for as long as `wait_secs` runs —
below the RFC 6762 §5.2 floor between two queries (`ASK_WAIT`) that the code
itself cites.

## Exit condition

A timer-free fix: set `on_the_link` only on a host-absent failure
(`not_yet_reachable`'s `EHOSTDOWN`, `EHOSTUNREACH`, `ENETUNREACH`, or a
failure seen after `WAITED`). A refusal or reset is the machine answering at
that address, so the next dial goes there again with no new ask, and only a
dial that finds nobody home asks the link once more.
