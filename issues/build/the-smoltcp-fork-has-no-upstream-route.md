---
status: assigned
kind: defect
opened: 2026-09-26
---

# The smoltcp fork has no route upstream

netd builds smoltcp from `ToyOSOrg/smoltcp` branch `toyos` (`forks.toml`,
`[smoltcp]`): v0.12.0 plus one change, the neighbour cache's discovery
silence kept per address rather than once for the whole cache. Without it a
connect to an on-link address nothing answers starves every other address's
resolution for as long as it asks (`netd_tcp_neighbour` reds on registry
smoltcp 0.12.0). `src/forkcheck.rs` holds a fork to carrying a change being
upstreamed, and this one cannot be sent as written: smoltcp follows NLnet
Labs' LLM policy (https://nlnetlabs.nl/llm-policy/), which refuses code an
LLM wrote and allows issues and discussion that disclose it. The branch
`neighbor-silence-per-address` holds the change on upstream `main`.

Owner: the owner, who posts the idea in smoltcp-rs/smoltcp#1209 with that
disclosure.

Exit condition: upstream fixes per-destination neighbour rate-limiting, netd
moves to the release that carries it, and the fork and its `forks.toml` entry
are deleted.
