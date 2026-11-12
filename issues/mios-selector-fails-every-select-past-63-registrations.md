---
status: open
kind: defect
opened: 2026-11-12
---

# mio's selector fails every select past 63 registrations

The mio fork's ToyOS selector (`src/sys/toyos/selector.rs`) makes its ring with
64 submission slots, and every `select` queues one watch per registration and
one for a change to them before it hands them over in one `inbox_submit`. Its
`queue` does not look at the ring's head, so a 65th watch overwrites the first
before the kernel has taken it, and the kernel refuses a submit of more than
its ring holds `InvalidArgument` (`kernel/src/inbox/mod.rs`,
`submit_submissions`). So while more than 63 registrations stand, every
`select` fails. A TCP stream registered for both directions takes two, its
receive bell and its bell (`src/net/tcp/toyos_stream.rs`, `register`), so a
tokio runtime with its waker and one listener fails at its 31st stream.

Found by reading, not exercised.

**Exit**: a `select` arms every registration however many there are, in
submissions the ring holds, and a test registers more sources than the ring has
slots and sees each one's event.
