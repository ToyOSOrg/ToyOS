# Userland

The module header at the site owns its subject — surfaces, translators and the channel in `toyos/`'s surface modules, soundd whole under `userland/soundd/`, what a process holds and how it got it at `kernel/src/object/` and `/system/bin/init`. The compositor's decisions are `toyos-desktop/`, pure and host-tested; `userland/compositor/` is devices, handles, shared memory and the panel. POSIX lives in `userland/libc` — ours, not a fork; that layer may be ugly, the kernel may not.

**A server never blocks on a client** — the doctrine no single site owns. Accept and the first frame are two events; a frame is buffered until whole before anything acts on it; a write is one `try_send` whose refusal drops the peer by name; a blocking read or write of a pipe the client owns is the same bug. init, the compositor, netd, soundd and every surface host use `ipc::FrameRx`. filepicker violates it today.

## Caveats that bite every agent

- **Nothing composes against the scanout** — reads from it miss every cache, which is why `window::Screen` has no read path. WC is weakly ordered: a blit ends with an `sfence` or the last partial buffer stays off the panel.
