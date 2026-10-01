---
status: expected-red
kind: tooling
opened: 2026-10-01
---

# The syscall-window storm times its victim by the guest's wall clock

`syscall_window_nmi_controls` red at 446 s in `648-648-whole.log`
(`wt/toyos-proclife1` `60ec86df3`, load average 84 on 14 cores): "the capture
has no `syscall-window-nmi: held cpu=` line and no `syscall-window-nmi: held
nobody` line". The storm never fired. It fires once one CPU has counted a
million syscalls (`nmi_gate::SPINNING_SYSCALLS`), and the spinner stops after
ten seconds of the guest's clock: under TCG that is the host's, and the
starved spinner said `950000 syscalls` (`syscalls: pid=9 total=950009 ...
cpu=9997ms`) — the dev host's quiet rate is a million in about 190 ms.

The storm's own waits on its victim are the same mistake at 100 ms each:
`HOLD_ACK_NS` for the held CPU's acknowledgement, `HELD_DELIVERY_NS` for the
aimed NMI to be taken, and `HOLD_ACK_NS` again for the released CPU's next
syscall. A vCPU the host has not scheduled for 100 ms answers none of them,
and the control's held arrival is then not the one the hold arranged.

**Exit**: the spinner spins until the storm is over, and every wait the storm
puts on its victim is a bound on a dead CPU and not on a starved one; then the
row goes.
