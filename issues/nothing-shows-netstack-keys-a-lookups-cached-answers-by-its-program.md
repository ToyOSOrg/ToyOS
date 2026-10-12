---
status: open
kind: tooling
opened: 2026-10-12
---

# Nothing shows that netstack keys a lookup's cached answers by its program

netstack's DNS cache answers a program only from its own view, which
`system/netstack/src/serve.rs` names by the badge on the lookup's connection,
and the supervisor mints each program its own `netstack` connector. Host tests
hold the cache's rules, and none reaches either wiring. A change that passed
one view for every lookup, or minted one badge for every program, would turn
no test red: no guest test resolves a name (`https_get` dials an address), and
under QEMU's slirp the lease's resolver forwards to the host's, which reaches
the internet. The T14 read the wiring once, in #883's metal measurement.

Exit condition: a netcase guest test against a resolver the harness answers
(`issues/there-is-no-network-gate.md`'s `-netdev socket`) shows that a second
row's lookup of a name the first row holds sends a query.

Owner: `issues/there-is-no-network-gate.md`, whose `-netdev socket` the test
needs.
