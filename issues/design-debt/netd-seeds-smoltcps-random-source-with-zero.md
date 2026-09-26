---
status: open
kind: defect
opened: 2026-09-26
---

# netd seeds smoltcp's random source with zero

netd builds its interface from `smoltcp::iface::Config::new`
(`userland/netd/src/main.rs`, `main`), whose `random_seed` is 0, and never sets
it. smoltcp draws from that seed (`iface/interface/mod.rs`, `Rand::new`)
wherever it wants a random number: a TCP connection's initial sequence number
(`socket/tcp.rs`, `random_seq_no`) and the DHCP client's transaction ID among
them. Every boot of every machine therefore draws the same sequence, so an
off-path sender can predict the ISN of netd's connections (RFC 6528 asks for an
unpredictable one).

The resolver no longer rides on it: `toyos-dns` takes every query ID from the
kernel's random source.

Exit condition: `random_seed` is drawn from `toyos_abi::syscall::random` before
the interface is built, and a test shows two boots' first ISNs differ.
