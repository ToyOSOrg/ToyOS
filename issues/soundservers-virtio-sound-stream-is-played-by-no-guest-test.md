---
status: open
kind: tooling
opened: 2026-10-10
---

# soundserver's virtio-sound stream is played by no guest test

No guest test plays a stream through soundserver's own virtio-sound driver
(`userland/soundserver/src/virtio.rs`, driven by the device loop in
`userland/soundserver/src/mix.rs`). `iommu_virtio_platform` reaches its
bring-up and stops there; `toyos-virtio-sound`'s host tests reach the driver
crate, not the interrupt-driven loop. Three behaviours of that path go unseen:

- the order of its lines: `virtio-sound: stream 0 started` once, after the
  client connects, then `virtio-sound: stream 0 stopped` once, then
  `soundserver: suspended`;
- the suspend comes only once every submitted period has completed
  (`unplayed == 0` in the suspend block of `mix.rs`);
- the suspend is reached through the claim's interrupts: with no client the
  loop's wait has no timeout, so a transmit queue with no vector leaves the
  stream `running`.

`virtio_sound_counts` asserted all three, beside a count of submitted against
filled periods that host scheduling sets; it was deleted whole for the count.

Owner: the orchestrator, which dispatches it. Exit: a QEMU guest test plays a
stream through soundserver's virtio-sound driver and asserts only the three
behaviours above, by order and completion, never a count or a time beyond a
hang ceiling; and the test reds when the transmit queue's vector is withheld.
