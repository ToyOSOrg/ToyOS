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

## Every remote login shares the session of the shell that started sshserver

No shipped image starts sshserver at boot (`src/build.rs`,
`no_shipped_boot_config_starts_sshserver`), so on a shipped machine it is
detached from a desktop shell and runs in that shell's login session. Its
`login` row opens no session there, so every remote login's programs spend
one share with each other and with the terminal that started the server: one
login at its parts has every other's next connection or stream on that server
refused, and that terminal's. Only where a boot config starts sshserver as a
service (`tests/metalcase`) does each launch it makes
open a session of its own.

**Ruled** (owner, 2026-10-07):

- The goal is one session per login, each with its own share and its own view
  of the files.
- The right to open a session is a capability the machine hands out: the
  supervisor starts sshserver as a machine service.
- Sessions are counted, they end, and their number is capped.
- Nothing inside a login session opens another.
- Until then the shared session stands, recorded here as a weakness.

Owner: stage 3 of `issues/every-program-sees-only-the-files-it-was-given.md`,
which removes it.

**Exit**: on a shipped machine sshserver runs as a service the supervisor
started, in the machine's session; two logins through it hold two shares, shown by a test in which one at its parts leaves the
other's first connection and first stream answered; a login past the cap is
refused by name; and a login's end ends its session.
