---
status: open
kind: track
opened: 2026-09-26
---

# The resolver refuses a truncated answer rather than asking over TCP

`toyos-dns` asks over UDP only and without EDNS (RFC 6891), so a server
answers in at most 512 bytes (RFC 1035 §4.2.1); a reply with TC set ends the
lookup with `Failure::Truncated`, which netd names in its log and answers
`ERR_OTHER`. An `A` question for an ordinary name does not reach 512 bytes:
of the names measured while the resolver was written, the largest reply was
161 bytes (`www.apple.com`, three aliases and one address). A name with a
long enough alias chain or enough addresses does.

To build: TCP fallback (RFC 7766) for a truncated reply, from netd's own TCP
stack, bounded by the lookup's existing waits.
