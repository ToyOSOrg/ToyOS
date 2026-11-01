---
status: open
kind: track
opened: 2026-11-01
---

# The internal panel stays lit with the lid closed

ToyOS has no lid handling. Closing a laptop's lid leaves the internal panel scanning out and its backlight on, which costs power and heats a closed machine. Both benches run with their lids closed.

The lid is a control-method lid device (`PNP0C0D`, ACPI 6.5 §9.4). Firmware signals a change with `Notify(lid, 0x80)`, and `_LID` answers whether the lid is open. On the T14 that device is `\_SB.LID0`. acpiserver evaluates the AML that raises the Notify, but nothing serves it: `deliver` counts it as "served by nothing" (`system/acpiserver/src/main.rs`).

The ownership is already set by the display track (`issues/each-gpu-drives-its-displays-from-a-userland-driver.md`): "Blanking on idle or lid" is the compositor's decision, and the driver carries it out. What is missing:
- acpiserver states the lid's state, from `_LID` at start and again at each Notify 0x80, to a client. Today it serves none (`issues/acpiserver-serves-no-client.md`).
- The compositor decides to blank the internal output while the lid is closed.
- The display driver powers the panel and its backlight down, and brings them back when the lid opens.

Not built now.

Owner: the orchestrator, for the display track.

**Exit**:
- A lid state change, from `_LID` or Notify 0x80, reaches the compositor. The internal output is blanked while the lid is closed and lit again when it opens. Host tests cover both, from modelled Notifies.
- On the T14, booted with its lid closed, the log shows the panel and backlight off.
