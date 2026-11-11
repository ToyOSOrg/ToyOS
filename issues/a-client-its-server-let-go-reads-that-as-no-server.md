---
status: open
kind: defect
opened: 2026-11-11
---

# A client its server let go reads that as no server

A server lets a connection go unanswered when it sent no whole request within
its limit (`toyos::admission`, `sdk/toyos/src/admission.rs`), as nginx closes a
connection whose header never came. The client sees a hang-up, and its library
names a hang-up as the service being absent: netstack's reads it as
`NetError::NetstackNotFound` (`hangup`, `sdk/toyos/src/net.rs`), the word for a
machine with no netstack, and a surface host's client as `GrabError::HostGone`
(`sdk/toyos/src/surface.rs`).

A connection refused for its holder's share is answered before it is closed
where the protocol has a refusal: `ERR_RESOURCE_EXHAUSTED` by netstack,
`TooManyConnections` by a file server, `Exhausted` by the block service. The
supervisor, the compositor, filepicker, logkeeper and usbd close it with a
line in their own log and no answer.

Not seen since the limit became 20 s; under the 2 s limit it was seen once,
`netstack_socket_churn` at `be929d941`, where a connect answered
`NetstackNotFound` was asserted `ResourceExhausted`.

**Owner**: the orchestrator, which dispatches the fix.

**Exit condition**: every client library of a server that admits through
`toyos::admission` names a hang-up before its answer as the server letting it
go, never as no server, and a host test of each mapping holds it.
