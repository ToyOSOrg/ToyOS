---
status: open
kind: defect
opened: 2026-09-08
---

# netd drops the tail of a client's write when smoltcp will not take all of it

`userland/netd/src/main.rs`'s `bridge_piped` moves a client's bytes from the
kernel pipe into the socket like this:

```rust
Ok(n) => { let _ = socket.send_slice(&buf[..n]); }
```

`tcp::Socket::send_slice` answers **how many bytes it enqueued**, and it takes
fewer than it was offered when the send buffer (`TCP_SOCKET_BUFFER`, 64 KiB) has
less room than that. The return value is discarded, so those bytes are gone: the
client was never told, the pipe has already given them up, and the stream the
peer receives is short in the middle with nothing anywhere saying so.

The read direction has the same shape one line above — `write_nonblock` into the
client's rx pipe, return value discarded — so a full pipe silently truncates what
arrived off the wire too.

Found while building `logd`'s record stream (`tests/common/logstream.rs`), whose
whole oracle is that what a listener received is the guest's own log file line
for line. It did not fire there: `logd` offers at most a batch at a time and the
64 KiB send buffer was never short of room on the arms that were run
(`log_stream`, 230 lines; `log_stream_e1000e`, 232). A client that writes in
larger units than netd's buffer would see it immediately.

The fix is to send what the socket took and keep the rest — the pipe is where
the rest belongs, and `can_send` is not the same question as "will take all of
this".
