---
status: open
kind: tooling
opened: 2026-10-12
---

# Nothing shows that netstack keys a lookup's cached answers by its program

netstack's DNS cache answers a program only from its own view, which
`system/netstack/src/serve.rs` names by the badge on the lookup's connection,
and the supervisor mints each program its own `netstack` connector, badged
with its row and its session. Host tests hold the cache's rules, and none
reaches either wiring. A change that passed one view for every lookup, minted
one badge for every program, or minted a row's badge without its session, so
that two logins of one row shared answers, would turn no test red: no guest
test resolves a name (`https_get` dials an address), and under QEMU's slirp
the lease's resolver forwards to the host's, which reaches the internet. The
T14 read the row half once, in #883's metal measurement; nothing has read the
session half, since that run's second row ran in the first's session.

Exit condition: a netcase guest test against a resolver the harness answers
(`issues/there-is-no-network-gate.md`'s `-netdev socket`) shows that a lookup
of a name one row holds sends a query when a second row asks it, and when the
same row asks it in a second session.

Owner: `issues/there-is-no-network-gate.md`, whose `-netdev socket` the test
needs.
