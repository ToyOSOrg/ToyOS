---
status: open
kind: defect
opened: 2026-08-19
---

# The panel once took no pixels for two minutes under load

First sighting, CI, one red against one green isolated re-run in the same job.
`src/redlist.rs` carries the row.

**What the harness printed** (PR #135 run 32303408773, `guest (11)`):

> `FAIL screen_console_clear: the graffiti actuator did not reach the panel:
> 0 of 2073600 pixels are [0, 192, 0] and the 8px strip below the cells is
> not` — at 127 s against a fast-tier test, so the shape is not a wrong pixel
> but a panel that never received the write inside a window two orders above
> its price. `ALONE: GREEN, and it was alone both times.`
> The same red run's `durations` job then refused the 126,762 ms measurement
> against the 10,000 ms fast line — correctly: that number is this stall, not
> the test's price, and it must never be committed as one.

**The family, and what half of it turned out to be.** The same evening's
`screen_console_panic` sighting looked like a fatal report losing the screen
under load, and this looked like an ordinary write losing it — one shape,
composition under a loaded host. That reading is dead for the other half: on
2026-08-24 both `screen_console_panic` captures were re-read and the command had
never reached the shell, because QEMU's 16-byte PS/2 queue drops what a guest
that is not draining cannot take, silently. Nothing about the panel was
involved. **This test types `test_rs_test_screen_graffiti` the same way**, and a
mangled command name is exactly what `0 of 2073600 pixels are [0, 192, 0]` looks
like — so read the panel out of the failing job before assuming a pixel was
lost.

**That mechanism is now closed for this test as well, and it reproduces this
message on demand.** `screen_console_clear` types both its commands through
`console_type_line`, which splits a line into bursts no wider than the device
queue and waits for the console to echo each one; the echo is matched as a
*prefix* of the input row, which is what lets the second command be typed onto a
panel the graffiti actuator has painted green. Staged the other way — the whole
command in one transmission, which is what a guest that drains nothing sees —
this test printed `the graffiti actuator did not reach the panel: 0 of 2073600
pixels are [0, 192, 0]` on 3 of 3 dev-host runs, verbatim the sighting above.
What that does not settle is whether the CI sighting *was* that: its capture was
not kept, and a lost pixel and a mangled command name print the same sentence.

The diff each rode on could not have caused it: PR #135 changes a tier
declaration and a duration table.
