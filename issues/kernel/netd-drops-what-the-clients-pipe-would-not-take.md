---
status: open
kind: defect
opened: 2026-09-08
---

# netd drops what arrives off the wire when the client's pipe will not take it

`userland/netd/src/main.rs`'s `bridge_piped` moves what the wire delivered into
the client's rx pipe like this:

```rust
Ok(n) if n > 0 => {
    let _ = toyos_abi::syscall::write_nonblock(pipe.as_handle(), &buf[..n]);
}
```

`write_nonblock` answers **how many bytes it took**, and it takes fewer than it
was offered when the pipe is short of room. The return value is discarded, so
those bytes are gone: `recv_slice` has already consumed them from the socket,
the client is never told, and the stream it reads is short in the middle with
nothing anywhere saying so. A client slower than the wire is exactly when it
fires.

The send direction had the same shape and no longer does: it now takes out of
the pipe only what the socket has room for, so nothing is consumed from one side
without landing on the other. The same answer does not fit here yet — it needs
the pipe's remaining room, which no syscall answers today.

The send side's reproduction — a record arriving cut in half at a peer that
had stopped reading — was an arm of the outbound log stream, which is gone; a
reproduction of this half is owed with the fix.
