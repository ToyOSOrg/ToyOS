---
status: open
kind: defect
opened: 2026-09-23
---

# netd's socket ids are numbers any client can name

A netd socket is named by a `u32` id netd hands out, and every request that
acts on a socket — close, shutdown, accept, send, receive — names it by that
id alone. netd checks that the id is in its table and nothing else: **it does
not ask whether the client naming it is the one it was issued to.** Clients
open a fresh connection to netd per request (`toyos::net::NetdConn::connect`),
so there is no client identity for the check to be made against. Any process
holding the `netd` connector can close, shut down or accept on any other
process's socket by guessing its number — and until 2026-09-23 the numbers
counted from 1 in every netd, so guessing was reading.

**How it was found.** A service swap (`toyos-swap`) replaces netd while its
clients still hold ids from the netd before. sshd, on noticing, dropped its
listener, which closed the listener's old id — a number that, in the new netd,
was `logd`'s stream connect still waiting for its SYN-ACK. netd removed that
socket from smoltcp's set and left the pending connect holding its handle, and
its next pass panicked: `smoltcp-0.12.0/src/iface/socket_set.rs:116:21: handle
does not refer to a valid socket` (`tests/swapcase`, `swap_crash_rolls_back`,
one run of four). A client's words crashed netd.

**What changed and what did not.** A close now answers a pending connect's
client instead of leaving its handle behind, so no id a client names can panic
netd that way; and netd's ids start at a random point, so two netds' ranges
overlap only by chance. Both bound the harm. Neither is the fix: an id is still
ambient authority over somebody else's socket.

**Exit condition.** A socket is named by something only its owner holds — the
data or notify pipe that already travels with it, or a per-socket handle — and
a request naming a socket its connection does not hold is refused. That is an
ABI change to `toyos::net` and netd's wire protocol, and lands on its own.
