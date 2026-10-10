---
status: open
kind: defect
opened: 2026-10-10
---

# The xAPIC ICR send polls no Delivery Status, and no machine has read it

`arch::apic::send` writes an xAPIC ICR's destination half and then its command
half without first polling Delivery Status (bit 12). AMD APM Vol. 2 §16.5
lets software write `ICRL` again without polling it, and Intel SDM Vol. 3A
§13.6.1 states no wait; Linux's `default_send_IPI_single_phys` polls anyway.
Whether a send can still be pending is the hardware's, and TCG
(`desktop_without_x2apic`) emulates none of it. The one xAPIC machine that has
run this path is the AMD Zen 2 laptop, which booted 16 of 16 CPUs through the
INIT-SIPI-SIPI sends and ran to its power-off, but its reading counts no
machine-wide shootdown, whose all-but-self IPI is the send
that would race a previous one: the stop says the census, `tlb: shootdowns=…`
among it, after logkeeper's last write (`syscall::machine`), so the laptop's
`/log` cannot carry it.

Owner: the orchestrator, which holds the laptop. Exit: a laptop boot's
`tlb: shootdowns=…` line, read off whatever channel carries the census
there, counts at least one shootdown, and that boot reaches its stop with no
`tlb: cpu … has not flushed` panic; or a lost IPI on it is traced to the
missing poll and the poll is added.
