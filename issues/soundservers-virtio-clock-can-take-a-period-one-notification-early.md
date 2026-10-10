---
status: open
kind: defect
opened: 2026-10-10
---

# soundserver's virtio-sound clock can take a period one notification early

`Sound::played` (`userland/soundserver/virtio-sound/src/lib.rs`) reads the
transmit queue's used ring only with the claim's interrupt record in hand, and
`Virtio::completion` (`userland/soundserver/src/virtio.rs`) hands the mix loop
that record's `last_nanos`. A record says when its notifications landed and
nothing about which period each was for. So when the device has written a
newer period's used element and its notification has not landed by the ring
read, that period is counted in the mask and stamped with the older
notification's time, and `dll.update(last_nanos, n)` in
`userland/soundserver/src/mix.rs` takes an update whose newest period is
clocked at least one period early: an error shaped like `n − 1` periods on
that update. Its notification then lands into the next record, which comes
back with no period.

How often the window opens is unmeasured, and no metal machine runs
virtio-sound. Nothing bounds the error but that rarity.

Owned by stage 1 of `issues/every-driver-is-still-in-the-kernel.md`: the
diff that moves HDA onto a `pci` claim feeds soundserver's DLL from a claim's
record (`kernel/src/pcidev/record.rs`) on every machine, and lands this exit
with it. Exit: the
record ties a time to the used elements it answers for — or the driver holds
back from a DLL update every period past those its record's notifications
answer for — and `toyos-virtio-sound`'s interleaving test,
`no_period_is_stranded_between_its_used_element_and_its_notification`, asserts
that no period comes back stamped earlier than its own notification.
