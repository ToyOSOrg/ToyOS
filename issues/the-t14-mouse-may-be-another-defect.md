---
status: open
kind: defect
opened: 2026-08-03
---

# A USB mouse on the T14 moved nothing for 28 s, and no boot since has read one

On the T14 compositor session of 2026-08-03 a USB mouse plugged in at 30.4 s
produced no motion for the rest of the boot
(`issues/t14-lost-every-integrated-input.md` records the same session's
integrated inputs). The kernel's xHCI driver that ran it is gone: `usbd`
drives every xHCI controller (`13ed7b4f4`), with its own HID path
(`system/usbd/xhci/src/hid.rs`), so nothing that driver did is evidence about
this one.

What that session left open, and where `usbd` stands on each:

- **Max ESIT Payload.** The old driver declared zero bytes per service
  interval for every periodic endpoint (xHCI 1.2 §6.2.3.8, §4.14.2). `usbd`
  writes the endpoint's payload into dword 4 for a periodic endpoint
  (`system/usbd/xhci/src/configure.rs`). QEMU has no bandwidth scheduler and
  never reads the field, so no host or QEMU test tells the values apart.
- **A first transfer that completes with an error, or an endpoint never
  scheduled at all.** That session's log could not tell them apart, and
  `usbd`'s does not either. A transfer that ends with an error code is
  restarted, and `usbd` names the code only when the endpoint will not run
  again (`system/usbd/xhci/src/hid.rs`). An error the restart clears says
  nothing, and neither does an endpoint that never completes.
- **`SET_IDLE`.** The old driver never sent it; `usbd` sends `SET_IDLE(0)` to a
  keyboard and says a refusal (`hid.rs`).

No T14 boot has had a USB mouse read under `usbd`: its T14 rows serve the boot
stick and drive the Type-C controller, and none binds a pointer.

**Exit**: a T14 boot with a USB mouse plugged in whose `usbd` lines show the
pointer's reports reaching the owner of input. Or, where none arrive, the
boot's `usbd` says whether the endpoint completed transfers with an error
code, naming the code, or completed none at all.
