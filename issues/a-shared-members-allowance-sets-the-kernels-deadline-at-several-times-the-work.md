---
status: open
kind: tooling
opened: 2026-10-09
---

# A shared member's allowance sets the kernel's deadline at several times the work, and no reading holds it

`toyos_tco::RUST_MEMBER_MS` and `toyos_tco::C_MEMBER_MS` are what one member
of a metal shared boot adds to its list's bound (`toyos_tco::list_bound_ms`).
The kernel's `boot-deadline=` is twice that bound and its hard-lockup bound is
half the deadline, so the two constants set how long a failed shared boot holds
the T14. They are 300 and 100 ms by the owner's ruling, not by a derivation:
nothing ties them to what the machine measures, and nothing reds when the
measurement moves.

## Readings

The T14 at `eff8b20ee`, three boots, armed with the 860 and 260 ms the
allowances were then; the judge's own lines:

| boot | members | ms a member | the list's last record | allowance now | factor |
|---|---|---|---|---|---|
| `shared` | 88 | 80 | 8 264 ms | 300 | 3.7 |
| `ccorpus` | 137 | 12 | 2 864 ms | 100 | 8.3 |
| `shared-debug` | 7 | 32 | 1 380 ms | 300 | 9.4 |

Each last record counts from the kernel's zero, so it holds about 1.2 s of
boot before the first job.

## The price, on failure only

What `--metal --list` and `metal::return_secs` give at 300 and 100 ms, against
the 60 000 ms list bound and 120 000 ms deadline of a boot no member rides:

| boot | a hung member, and a CPU that takes no interrupt, hold the machine | a wedged kernel | a boot that never returns is waited |
|---|---|---|---|
| no member | 60.0 s | 120.0 s | 420 s |
| `shared-debug` | 62.1 s | 124.2 s | 425 s |
| `ccorpus` | 73.7 s | 147.4 s | 448 s |
| `shared` | 86.4 s | 172.8 s | 473 s |

**It grows with every member and has no ceiling**: 0.6 s of deadline a Rust
member and 0.2 s a C case. The 88 Rust members and 137 C cases on one boot,
which is what joining `shared` and `ccorpus` to `testcases` makes, arm a list
bound of 100 100 ms and a deadline of 200 200 ms, waited 501 s. A clamp is no
answer: a ceiling under the derived bound ends a healthy list.

**A late expiry adds to the longer bound.**
`issues/a-120000-ms-boot-deadline-fired-132859-ms-late-on-the-t14.md` is open:
that deadline was reached at 252 859 ms, and the same lateness on `shared` is
305 659 ms. The wait moves with the deadline, so what is left of it after such
an expiry is the same 167 s; the machine is held 52.8 s longer.

## Owner

The metal suite: `tests/common/metal.rs`, which derives the bounds, and
`toyos-tco`, which declares the allowances.

## Exit

Both, on the T14:

- Each allowance is derived at its declaration from the T14's measured
  milliseconds a member by one stated factor, and the judge reds a shared boot
  whose list's last record came past half its bound, which today is a line a
  reader reads.
- The boot that carries `shared`, `ccorpus` and `testcases` together is armed
  with a `boot-deadline=` of at most 240 000 ms, twice what a boot no member
  rides carries, and its list's last record comes within half its bound.
