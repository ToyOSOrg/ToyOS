# System programs

The module header at the site owns its subject — surfaces, translators and the channel in `sdk/toyos/`'s surface modules, soundserver whole under `system/soundserver/`, what a process holds and how it got it at `kernel/src/object/` and `system/supervisor/`. The compositor's decisions are `system/compositor/desktop/`, pure and host-tested; `system/compositor/` is devices, handles, shared memory and the panel.

**A server never blocks on a client** — the doctrine no single site owns. Accept and the first frame are two events; a frame is taken whole with `ipc::FrameRx::pump`, which never waits; a write is one `try_send` whose refusal drops the peer by name; a blocking read or write of a pipe the client owns is the same bug. The supervisor, the compositor, netstack, soundserver and every surface host use `ipc::FrameRx`.

## Caveats that bite every agent

- **Nothing composes against the scanout** — reads from it miss every cache, which is why `window::Screen` has no read path. WC is weakly ordered: a blit ends with an `sfence` or the last partial buffer stays off the panel.
