---
status: open
kind: defect
opened: 2026-08-15
---

# A wait was registered while its task still read *waiting*

The kernel panicked three times on the assertion that a task waits on at most
one queue, now `Watch::register`'s `assert!(task.set_waiting(), "a task waits
on at most one watch")` (`kernel/pure/sched/watch.rs`). What the assertion
reports is a `waiting` flag (`TaskShared::set_waiting`,
`kernel/pure/sched/task.rs`) left set by an earlier wait of the same thread.
Which wait left it set was never established.

## The sightings, all on paths since removed

- Once in logd's `io_uring_enter` on a KVM shard (nightly dispatch
  `31900050723`, job `95049280131`, 2026-08-15), 4.7 s after the boot stick
  was pulled and the kernel's usb-storage gave the disk up. The same tree was
  green on the same name minutes earlier; one guest per machine, so host
  contention does not explain it.
- Twice from a host-side keyboard flood of a few thousand key events a second
  into the in-guest runner blocked on stdin at `===READY===`, through
  `scheduler::wait_until::<kernel::keyboard::has_data>`, once with the i8042
  present and once with `q35,i8042=off`. Not at ordinary typing rates.

`io_uring` and the shared wait queues went with the one-watch rewrite
(`aaddf38a7`, `232f69bb6`), and the kernel's keyboard with the i8042 and USB
drivers leaving it (`febfd21e8`, `13ed7b4f4`), so none of these routes exists.
`usb_boot_stick_pulled` is deleted (`02366d741`).

One way in was found and closed in the primitive: a wake popped a waiter and
cleared its flag in two steps, so a waiter withdrawing between them found its
own flag still set (`aaddf38a^:toyos-sched/loom/tests/loom_ticket.rs`,
`cancel_and_wake_agree_on_who_won`). That was not shown to be the sightings'
path, and nothing has reproduced the panic since.

**Exit**: either a reproduction on the current watch, accounted for by the
wait that left the flag set and fixed there, or a loom model of
`Watch::register` and `unregister` over every exit of a wait (commit, cancel,
revoke) that shows each clears the flag, with a mutation that leaves it set
going red.
