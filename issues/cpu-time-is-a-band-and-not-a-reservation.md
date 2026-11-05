---
status: none
kind: rejected
opened: 2026-08-14
---

# CPU time is a band, not a reservation: reservations are rejected

**Declined by the owner on 5 November 2026**, answering the open decisions of
#1191: ToyOS's scheduler is fair scheduling, EEVDF by job, with a per-CPU
real-time allowance. CPU time is not reserved.

The declined proposal made every entity hold a budget and a period: a per-CPU
ledger with admission arithmetic, real-time clients as earliest-deadline
reservation clients, a dying server per CPU, a reserved urgency mark,
reservation rows in the system manifest and a service invariant gated against
them. Its full text is in this file's history.

The scheduler's stages are `issues/the-scheduler-and-the-clocks-never-talk.md`
(owner decisions J1 and J2). The reservation model was to replace the dying
corpses' per-corpse deferral; that weakness is
`issues/k-dying-corpses-take-k-deferral-grants-on-one-cpu.md`.
