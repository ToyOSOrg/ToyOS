---
status: open
kind: defect
opened: 2026-09-27
---

# A disk whose port went away panics the boot at ROOT's hold

`usb_transport_break` (nightly tier) fails its boot with:

    PANIC: panicked at src/rootfs.rs:177:19:
    boot: the partition ROOT was read from, <guid>, cannot be held: Unusable

`rootfs::hold_source` (#506, 265a0fce) runs after the USB gate. It asks
`gpt::claimable` for ROOT's partition, and that reads every registered disk's
table. The test arms `usb-port-gone`, which ends the gate's last read on its
disk as a port that read disconnected and leaves the disk to its port's
teardown. That disk's table read then fails, `table_refused` answers
`Unusable`, and `hold_source` panics, although ROOT is on the other stick.

Measured at 9583d913 merged with 5e446e5c:
- `cargo test --test toyos-build -- --nightly usb_transport_break` EXIT=1,
  red alone as well.
- The same run with `usb-port-gone` removed from the test's `PARAMS`, as a
  checked patch that was restored: the boot no longer panics, and the test
  fails only on the port-gone assertion it no longer staged.

The nightlies agree:
- Red the same way on main's nightly at e8d7c9c0 (run 36228604597).
- Red at fd62f567 (run 36278449733, guest (7)).
- On the nightly at 3f46a019 (run 36111884575, before #506) it failed for a
  different reason.

`cargo run -- --known-red usb_transport_break` answers NO.

**Exit**: a disk that left during boot is not a disk ROOT's hold must read,
or the boot's refusal is what `usb_transport_break` expects. Either way
`usb_transport_break` is green on a nightly.
