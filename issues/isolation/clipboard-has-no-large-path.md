---
status: open
kind: defect
opened: 2026-08-08
---

# The clipboard's sender stops inlining at 4096 and its receiver keeps 116

Filed out of the SDK IPC-framing entry when that closed; the framing fix did not
touch this.

**The large path works now, and the surviving defect is the small one.**
`clipboard_set` (`userland/window/src/lib.rs`) splits at 4096: below that it
sends `MSG_CLIPBOARD_SET` with the bytes inline, above it allocates a
`SharedMemory`, copies the text in and *moves the region's handle* with
`send_with_handles(MSG_CLIPBOARD_SET_SHM)`. The compositor adopts the handle,
bounds the client's declared length against `MAX_CLIPBOARD_BYTES` (2 MiB,
`userland/compositor/src/client.rs`) and refuses past it by name on stderr.

The inline half did not move with it. The compositor's `ClientRx` is
`ipc::FrameRx<MAX_KEPT_PAYLOAD>` with `MAX_KEPT_PAYLOAD` = 116, and `FrameRx`
reads whatever a frame declares past `KEEP` and discards it. So **text between
117 and 4096 bytes arrives as its first 116 bytes**, `session.rs`'s
`MSG_CLIPBOARD_SET` arm stores that, and neither side is told. Two numbers that
have to agree and do not: the sender's 4096 and the receiver's 116.

`MAX_KEPT_PAYLOAD`'s own doc comment states the assumption that used to make it
right — "`MSG_CLIPBOARD_SET`'s 116 bytes is the widest of them" — which the
sender stopped honouring when it gained the 4096 threshold.

The paste direction is the shape this one should copy: `session.rs` splits at
the same 4096, and `Window::poll_event`'s `MSG_CLIPBOARD_PASTE` arm reads into a
4096-byte buffer, so the two thresholds are the same number and nothing is lost.

**What is no longer true, recorded so nobody re-derives it.** Until 2026-08-24
this file said the shared-memory route "cannot work in that direction at all",
because `shared_memory::map` required membership in an `allowed` list, only the
owner could `grant`, and no syscall told a client its peer's pid. That whole
argument is gone with the ACL: a region is reached by holding its handle, the
sender moves the handle with the message, and the compositor's comment on the
`MSG_CLIPBOARD_SET_SHM` arm states the consequence — "the region itself is no
longer a claim: it is a handle the client moved, so there is nothing left to
disbelieve about which memory this is."

Two ways out, and they are not equivalent: make the sender's inline threshold
`MAX_KEPT_PAYLOAD` so everything above it takes the working shm path, or raise
what the compositor keeps. The first needs one constant to be shared instead of
two written down; the second buys a bigger frame nobody needs.
