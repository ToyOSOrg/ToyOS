---
status: open
kind: finding
opened: 2026-09-08
---

# `getsockname` answers an address nobody asked netd for

`userland/libc/src/socket.rs`'s `getsockname()` fills the caller's `sockaddr`
with `[10, 0, 2, 15]` and the socket's local port. The address is a literal in
that function; netd is not asked, and the SDK has nothing to ask it with —
`toyos::net` carries `tcp_connect`, `tcp_bind`, `tcp_accept`, the UDP calls and
`dns_lookup`, and no call that answers "what is this machine's address".

It was true by coincidence until now: netd carried the same literal, so the
shim and the stack agreed. netd takes its address from DHCP as of the change
that filed this, so the two agree only on a machine whose server happens to
lease `10.0.2.15` — QEMU's user-mode backend does, and the bench's router does
not. Every C program that asks what address it is bound to is told the wrong
one there.

What it costs to fix is a message type on netd's protocol and one plumbed
answer; what it costs to leave is that the one caller of `getsockname` in a
POSIX program is the one that then advertises an address nothing can reach.
Nothing in the tree reads it today, which is why this is a finding and not a
defect.
