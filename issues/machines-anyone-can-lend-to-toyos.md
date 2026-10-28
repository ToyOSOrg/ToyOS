---
status: open
kind: track
opened: 2026-10-11
---

# Machines anyone can lend to ToyOS

The metal loop needs nothing from a machine that the internet can't carry. A
machine anywhere, whether the owner's, a friend's or a community member's,
could boot ToyOS for the suite with no one standing next to it.

**What exists.**
- `--metal-host` targets any bench.
- `toyos-metal --record` captures a machine into the host store
  (`timings::dir()`), never into the tree.
- The loop writes the image to the machine's internal install area over
  `ssh` and sets a one-shot `BootNext`.
- The machine comes back to its own OS, and the loop reads the log partition
  back.

The second bench (an AMD laptop) went from first contact to a green row
through this loop with no hand on it.

**What a machine has to give.**
- An ssh-reachable Linux as its resident OS, and an account with
  passwordless sudo.
- A USB stick that stays plugged in.
- Firmware that honours an OS-set `BootNext` and falls back to the resident
  OS.
- Recovery from a hang without a hand. Today that is the boot deadline alone
  on machines without the Intel TCO watchdog or the architectural PMU; a
  switchable power outlet would add another way back.

**What it takes to make it anyone's, and is not built.**
- **Trust both ways.** We run commands as root on someone else's machine,
  and they run our signed images. Name what each side may do and how that
  is enforced.
- **Onboarding.** A capture someone else can run, and a record of the
  machine that names it by kind, never by an identifier.
- **Privacy.** Logs come back from machines we don't own. Machine
  identifiers (DMI strings, serials, BIOS versions, addresses) are stripped
  before anything leaves the orchestrator's host, by construction rather
  than by a mask written after the fact.
- **Hardware requirements.** A row names the hardware it needs, so a machine
  without it skips the row instead of failing it
  (`issues/a-metal-row-names-no-hardware-and-a-bench-without-it-reds.md`).
- **Scheduling.** Several machines, each with one boot at a time, and the
  results judged per machine.
- **Reachability.** Today it is a tailnet. Name what replaces it for
  machines outside it.

**Exit:** a machine whose owner the orchestrator has never met is captured,
runs a filtered metal suite and returns its result with no identifier in it,
with no person touching either end.

**Owner:** the owner (Jan) decides when it starts.
