---
status: open
kind: defect
opened: 2026-10-12
---

# One program that fills a server's table holds every other client out

A server holds the connections it has accepted and not yet served in a table of a fixed size, and
that size is one number for every client. The eight servers whose `HANDSHAKE_TIMEOUT` empties
their table (`issues/servers-let-a-client-go-on-the-handshake-limit-with-its-request-unread.md`)
let a silent connection go on that limit, so one program that connects again each time one is let
go keeps the table full for as long as it runs. soundserver's control plane, 63 slots
(`MAX_CONTROL_CLIENTS`, `system/soundserver/src/control.rs`), and a surface host's clients, 4
(`toyos::surface::MAX_CLIENTS`, `sdk/toyos/src/surface.rs`), have no limit at all: a program that
connects that many times and says nothing holds them until it hangs up.

Meanwhile every other client is let go at the full table, or, where the server stops accepting,
waits in the port's queue behind it. That record's exit has every full table stop accepting and
takes the limit's length from published defaults of 20 s to 60 s, so each other client waits up to
a whole limit behind the one program, and behind soundserver's or a surface's for as long as that
program runs.

Only fileserver shares its table out: one share holds at most `HANDSHAKE_SHARE`, a quarter of
`MAX_HANDSHAKES`, and its next connection is answered `ResourceExhausted` and let go
(`system/fileserver/src/main.rs:80-86`, `:455-462`). Its share is the one the grant on the
connection's badge names, which the supervisor mints per session, not per program
(`system/supervisor/src/main.rs`, `mint`). A badge naming the program, its row and session, is on
every connection to netstack and to the supervisor's launcher and grants ports
(`Grants::netstack`, `launcher_namespace`, `grants_namespace`); diskserver's names a partition, and
the others' connections carry none. `issues/netstacks-places-are-one-number-for-every-client.md` and
`issues/netstack-lookup-slots-have-no-per-client-share.md` record the same hole on netstack's places
and lookups. The supervisor mints the connector of every server it spawns, but not a surface
host's: console and terminal each create their own port and move its connector only into the shell
they spawn (`system/console/src/main.rs:154-156`, `system/terminal/src/main.rs:55-56`). Lines are at
`1a7464692`.

**Owner**: the orchestrator, which dispatches the fix.

**Exit condition**: every table named above holds at most a per-client share of itself, keyed by a
badge that names the client, written by whoever mints the connector: the supervisor for the servers
it spawns, console and terminal for their own ports; a client past its share has its next connection
refused by name while another client's is taken, and a host test of each table goes red when one
client holds more than its share.
