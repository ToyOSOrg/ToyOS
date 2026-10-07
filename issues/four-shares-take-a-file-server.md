---
status: open
kind: defect
opened: 2026-10-07
---

# Four shares take a file server

`userland/fileserver/src/main.rs` gives each share a grant names a quarter of
each machine-wide bound: 32 of `MAX_SERVED`'s 128 served connections, 8 of
`MAX_HANDSHAKES`' 32 waiting on their hello, 16 of `MAX_STREAMS`' 64 streams.
So four shares at their parts take each bound whole, and a fifth's first
connection or `STREAM` on that server is refused `ResourceExhausted`.

A share is a service the supervisor started or a login session
(`toyos_manifest::launch`), and neither gets a second by launching: a launch
opens a session only when a `login` row makes it from the machine's session.
What is left is how many shares there are. The shipping boot starts more than
four services that hold DATA's directories, and the compositor's row opens a
session for each program its taskbar starts, so four of them, each at its
parts, refuse the rest. Nothing counts the sessions open, and no session ever
ends (`issues/every-program-sees-only-the-files-it-was-given.md`, "Known
weaknesses").

The parts are sized against what was measured, not against a share's need:
every boot of the guest suite, the shipping desktop booted under QEMU, and
every shipping Rust job run one after another in one `tests/testcases` boot
never had one start past 3 served connections, 1 waiting on its hello or 0
streams, nor more than 5 of DATA's connections served at once. A parallel
build (cargo with rustc as its direct children) is the unmeasured load.

Owner: the file server.

## Exit condition

However many shares are held, each at all a server lets it hold, the next
share's first connection and first stream on that server are answered, or
opening the session that would hold it is refused by name; shown by a test
that does exactly that.
