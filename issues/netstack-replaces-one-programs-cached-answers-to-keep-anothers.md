---
status: open
kind: defect
opened: 2026-10-12
---

# netstack replaces one program's cached answers to keep another's

netstack's DNS cache keeps each program's answers in a view of its own, named
by the row and session its `netstack` connector was minted for, and holds at
most `toyos_dns::MAX_VIEWS` views. When a program with no view looks a name up
and every view holds a live answer, the least recently used view is replaced
whole. That view's program finds its next lookup of a name it held sent to the
resolver, and so learns that at least `MAX_VIEWS` other programs kept answers
since its own last lookup: a count of programs, never a name. Anyone can
multiply the tenants, since every ssh login opens a session, so a program can
also replace another's view on purpose, or keep views of its own and watch the
oldest go to count other programs' first lookups. A view whose answers have all
expired is replaced before a live one, which tells its program nothing.

No bound on one pool whose tenants anyone can multiply avoids this. The lookup
slots (`issues/netstack-lookup-slots-have-no-per-client-share.md`) and the
node's places (`issues/netstacks-places-are-one-number-for-every-client.md`)
share its root: what netstack holds for a program is charged to netstack, not
to the program.

Evidence: `toyos-dns`'s host test
`a_new_view_replaces_an_expired_view_first_and_else_the_least_recently_used`.

Exit condition: a view is charged to the program it serves, so netstack never
replaces one program's view to keep another's, and a test of the node in which
`MAX_VIEWS` + 1 programs each keep an answer finds the first program's next
lookup of its name answered with no query.

Owner: whoever holds `issues/toyos-has-its-own-network-stack.md`.
