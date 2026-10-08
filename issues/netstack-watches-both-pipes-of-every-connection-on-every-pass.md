---
status: open
kind: defect
opened: 2026-10-08
---

# netstack watches both pipes of every connection on every pass

Every pass of netstack's loop submits a watch for each pipe of each live piped
connection, ready or not, changed or not (`userland/netstack/src/main.rs`, the
loop in `main`): an idle connection submits two. A watch replaces its handle's
earlier one, so each is a poll allocated and registered again, with the takes
of the global pipe lock that `arm` makes to read the pipe and find its watches
(`kernel/src/inbox/mod.rs`).

A poll that asks `OTHER_END_GONE` sits on its own end's watch, which is the
watch that end's readers or writers park on. So every write a client makes
through the kernel posts the poll netstack keeps on that connection's send
pipe, and every read the poll on its receive pipe: netstack's submitter is woken in the kernel for a
look that finds the other end held and arms again. No completion is written and
no pass runs, and the wake is paid per client read and write.

Read from the code, not measured: nothing in the tree reads netstack's
throughput or its passes per second with connections open, and a duration is
the T14's to say.

**Exit condition**: a pipe's watch is submitted only when its interest changes
or the kernel has answered it, and a post that is no leaving wakes no
`OTHER_END_GONE` poll; or a throughput reading on the T14, the same transfer
with and without the watches, that shows the cost is nothing.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
