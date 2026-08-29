---
status: open
kind: defect
opened: 2026-08-29
---

# A guest that ran past-EOF hole writes wedges minutes later, with every task parked and nothing runnable

Found while closing `issues/kernel/lseek-past-eof-is-silently-clamped.md`, and
the reason that close is reverted rather than landed. The fix itself was small
— `ops::seek` stops clamping to EOF, one `MAX_FILE_SIZE` ceiling (2^44, the
`u32` page index's reach) refused by seek, write and ftruncate, and ftruncate
no longer moving the seek pointer — and its own test
(`lseek_past_eof`, POSIX-as-oracle, holes on /tmp and device-backed /home plus
the ceiling refusals) was green alone in 37 ms and green in its shared boot.

The shared boot then died. In 4 of 5 full fast-tier runs on the dev host
(2026-08-29, 12-wide, TCG) the boot carrying `lseek_past_eof` wedged minutes
after it passed: `port_poll_churn`, later in the same guest, never finished,
and the kernel's periodic reporter kept ticking with `ready=0 dying=0 parked=3
current=None` (one run: `parked=5`) for the rest of a 300 s budget — an idle
machine whose tasks are parked and are never woken again, not a slow one. One
of the 5 runs was green; `main` ran the same tier clean twice.

The attribution is measured, not argued:

- the same composition with the test **neutered** (same binary and name, `main`
  prints and exits) ran 301/301 green — so it is the test's filesystem
  activity, not its presence or timing;
- with the test's file removed from the tree the tier matched `main`'s profile;
- each half alone — the hole arms only, the ceiling arm only — was green once
  each (n = 1, against a wedge rate of ~4/5 for the whole test, so this
  localizes little).

What is *not* known: the mechanism, and whether the wedge is the fix's or a
latent one the fix unlocks — a write landing past EOF through a preserved seek
position was unrepresentable before it, so this workload has never run on any
earlier tree. The last captured kernel line before one wedge was an unrelated
thread's clean exit; the harness keeps only the last 60 lines, all reporter
noise, so where the first waiter got stuck is not in any capture taken so far.

The reverted fix and test are one commit — the revert names it — so the next
investigation starts by reverting the revert, running the fast tier (the wedge
came in 4 of 5 runs), and instrumenting the boot for who parked first; suspect
the write-back path first, since a wedged `iod` parks every later spawn at its
binary open, which is the shape of a whole boot going quiet.
