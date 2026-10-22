---
status: open
kind: defect
opened: 2026-10-05
---

# std says a provided connector leaves the parent only on a successful spawn

A spawn that provides a connector goes through the launcher, and the send moves
every handle it names: the kernel moves them to the launcher, or refuses the
move and `Connection::send_with` closes them in the caller. Every answer but
`LaunchError::NotSent` is past that point (`Spawn::ask_launcher`,
`sdk/toyos/src/spawn.rs`, which says so).

std's documentation of the same handles says otherwise.
`CommandExt::provide` (`sdk/std/os/process.rs`) says the connector is moved
"like `endow`", whose rule is that the handle leaves the parent "after a
successful spawn". A provided connector also leaves the parent on a spawn that
failed after the send (a refused move, `Refused`, `Gone`, a lost answer), and
std's caller cannot keep it. An endowed spawn goes through `SYS_SPAWN`, never
the launcher (`route`), so `endow`'s own rule holds.

**Owner:** the next commit that touches `sdk/std/os/process.rs`.

**Exit:** `provide`'s doc says the connector leaves the parent once the launch
is sent, whatever the spawn answers.
