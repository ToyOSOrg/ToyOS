---
status: open
kind: defect
severity: [security]
opened: 2026-10-12
---

# One program that fills a server's table holds every other client out

Every server but two keeps the connections it has accepted and not yet served
in `toyos::admission`'s table (`sdk/toyos/src/admission.rs`), where one key
holds at most a share and a silent connection is let go at the limit. The key
is the badge on the connection, and the share protects only where that badge
names the holder:

- **Shared out by holder**: a file server's hellos, by its grant's share
  (`system/fileserver/src/bounds.rs`, `HANDSHAKES`); netstack's requests
  (`system/netstack/src/client.rs`, `ASKING`) and the supervisor's `launcher`,
  `grants` and `admin` (`system/supervisor/src/main.rs`, `ASKING`), by the row
  and session the supervisor minted the connector for; the supervisor's
  `login` by the row alone (`opener`), so every session of one `login` row
  spends one share.
- **Shared out by grant, not holder**: the block service's
  (`system/diskserver/src/main.rs`, `ASKING`). Its badge names what is reached,
  so two holders of one grant spend one share.
- **One share for every holder**, since the connector carries no badge: the
  whole table of the compositor's `compositor` port
  (`system/compositor/src/client.rs`, `CLIENTS`), logkeeper's `log`, usbd's
  `inspect`, netstack's `link` and the supervisor's `power`; half of
  filepicker's for each of its two ports; and the quarter of the supervisor's
  shared table that `swap`'s holders spend. One program that connects again
  each time a connection of its own is let go keeps such a share full for as
  long as it runs, and every other client of it waits in the port's queue: a
  stop behind eight silent `power` connections waits up to the 20 s limit.
- **Not on the mechanism, and with no limit**: soundserver's control plane, 63
  slots (`MAX_CONTROL_CLIENTS`, `system/soundserver/src/control.rs`), and a
  surface host's clients, 4 (`toyos::surface::MAX_CLIENTS`,
  `sdk/toyos/src/surface.rs`). A program that connects that many times and
  says nothing holds them until it hangs up. Both keep a client after its first
  request, so a share of the waiting table alone does not bound them: their
  served clients need the share too.

The supervisor mints the connector of every server it spawns, but hands out
one unbadged connector per port to every row that receives it
(`received`); console and terminal each create their own surface port
and move its connector only into the shell they spawn. A program that holds
several sessions, or that launches other rows, holds as many keys:
`issues/four-shares-take-a-file-server.md`.

- **Owed on metal:** the boots `boot:testcases` (its `testcases` and
  `testcases-watchdog` images) and `boot:shared-debug`, on the T14; the
  servers' admission through `toyos::admission` has run on no metal yet. Main
  must show every job of those boots passing within twice its recorded time;
  no line from the supervisor, the file server, diskserver, logkeeper or usbd
  refusing a share, a full table or a silence to a client of the boot's own;
  and diskserver's `disk driver … (label) connected` for each driver.

**Owner**: the orchestrator. Badging every connector with its holder is a
change to what a badge says (the manual: what was granted, never who
connects), which is the owner's to decide.

**Exit condition**: no one holder can take more than its share of any table
named above, however its connector came to it; soundserver's control plane and
a surface host admit through `toyos::admission` and bound their served clients
the same way.
