---
status: open
kind: defect
opened: 2026-07-31
---

# `MOUSE_WATCH` and `NETWORK_WATCH` are posted for nobody's benefit

Re-measured against `739af0c2` on 2026-08-24, after §5.6 replaced the wait
queues with completion `Watch`es. The entry's first half is answered; its second
is exactly as filed, in the new mechanism.

**Answered: the asymmetry is now a decision, stated where it is made.** A
blocking read of an empty Keyboard claim parks and one of an empty Mouse claim
answers `NotFound`, and `read_block_device`'s doc
(`kernel/src/arch/syscall.rs:1220-1229`) says why rather than leaving it as an
accident: "Only these four wait. A mouse, a NIC and a framebuffer answer
`NotFound` to a blocking read that has nothing — which is what they did as
descriptors, and what their holders already build around." The entry said "pick
one"; one is picked. The device read itself is symmetric — `ops::read_device`
(`kernel/src/object/ops.rs:354-378`) returns `None` for an empty queue on both —
so the whole of the difference is that one line.

**Still true: two of the four device subjects are posted and nobody arms on
them.** The wakes exist:

- `MOUSE_WATCH` — `mouse::wake_waiters` (`kernel/src/mouse.rs:160-162`), called
  from `i8042/mod.rs:892` and `xhci/hid.rs:147`.
- `NETWORK_WATCH` — `net::wake_waiters` (`kernel/src/net.rs:52-54`), called from
  `sched/driver.rs:1027`.

The arms do not. Every `completion::Subject::of` over a `waitqs` static in the
kernel is one of three: `AUDIO_WATCH` twice (`syscall.rs:1312`, `:1327`) and
`KEYBOARD_WATCH` once (`:1342`). Nothing ever parks on `MOUSE_WATCH` or
`NETWORK_WATCH`, so both posts walk an empty watcher list on a hot path — the
i8042 drain and the xHCI HID completion for one, the scheduler pass's network
poll for the other — and this is a direct consequence of the decision above:
because an empty Mouse read answers `NotFound` rather than blocking, there is
never a parked mouse reader to wake.

**Both are separate from the inbox path and cannot be standing in for it.**
`mouse::inbox_watchers` and `net::inbox_watchers` are their own lists, walked by
their own callers; deleting a `Watch` post does not touch a poll registered
through an inbox.

So the remaining work is one of the two the entry named, and the other is now
foreclosed: with `NotFound` ruled deliberate, deleting these two wakes — and the
two statics with them, if nothing else claims them — is what makes the mechanism
honest. `AUDIO_WATCH` and `KEYBOARD_WATCH` stay; they have arms.
