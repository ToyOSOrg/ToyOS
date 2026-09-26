---
status: open
kind: defect
opened: 2026-09-26
---

# toybox's `net` applet cannot reach netd

`/bin/net` (`userland/toybox/src/net.rs`) fetches a URL with
`TcpStream::connect`, but `[programs.toybox]` in `system.toml` receives
`compositor`, `soundd` and `surface` and not `netd`. Every connect it makes is
answered `NetdNotFound` by the SDK, so the applet can only ever print
`net: connection failed`. Endowing the row with `netd` would hand every other
toybox applet the network too, the same granularity `system.toml`'s own comment
on the row names.

Exit condition: `net` fetches from a host, or is removed.
