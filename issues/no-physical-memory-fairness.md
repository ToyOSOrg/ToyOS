---
status: open
kind: defect
opened: 2026-07-30
---

# No physical memory fairness

Any ordinary job can take memory until the machine runs out, and every other
ordinary job is refused at its next call meanwhile. Nothing signals pressure,
and nothing a person can reach stops the job that took it.

**What holds now.** Everything a process causes is counted to its job's
account (`kernel::frames::Ledger`, read through `account_info`), and the
control paths' jobs keep a floor no other job's allocation can take
(`memory-floor-mib` in `images/system.toml`: the supervisor and ps2server).
A floor is a protected total allowance: the first `floor` of the job's use
is guaranteed, cgroup v2 `memory.min`'s contract, not an emergency
allowance on top of it (owner, 2026-10-24). Attribution imposes no per-job
limit. A service whose
restart is refused for memory keeps its ports and starts again once memory
comes back, which its job's readiness says. `memory_floor_holds_the_controls`
(QEMU) and the `memory_floor` metal row hold, with one ordinary job holding
every frame: ps2server's floor carrying a key to the owner of input, a
system holder killing the greedy job, and a starved service serving again.
**No test establishes the supervisor's floor**: with only it removed the
test passes. Without it, a service that dies under exhaustion, such as
fileserver refused its writeback, can be refused its restart for the
supervisor's own memory, which closes it (below).

**What is still missing, and is this entry's exit:**

- **The stop UI.** Under exhaustion nothing a person holds can find and stop
  the greedy job, and no exhaustion recovery is claimed for visible controls
  or for input: the guest test stands a system holder in for them. The
  compositor has no floor (owner, 2026-10-24). Owner: the compositor and
  memory tracks. Next: a bounded emergency path in the existing compositor,
  in which ordinary rendering cannot spend protected capacity, and a review
  of the whole path from input to the visible controls to the stop, USB
  input included, before any recovery is claimed.
- **What the supervisor holds grows with the sessions and programs it
  runs**, and on its floor. A login authority connects its own control
  connection and an owner its own link, and a launched program's image is
  not the supervisor's (its server's object, or a copy its own job holds),
  but each live program still holds a few KiB of the supervisor's
  (its handle, notice and the connectors minted for it), bounded only by the
  process table. Owner: the supervisor track. Exit: what a launch makes for
  the child counted to the child's job, or a bound on live launches the
  floor is sized for.
- **A restart refused for the supervisor's own memory closes the service
  for good.** The supervisor waits for memory only where the service's own
  job recorded the refusal (`start_again`); where the supervisor's job was
  refused, such as mapping a boot image for the child, the service is closed
  instead. Owner: the supervisor track. Exit: a start refused for
  the supervisor's own memory waits on the supervisor's own job's readiness,
  as one refused for the service's waits on the service's.
- **Connect-time capacity.** Both rings of a connection are its connecting
  job's, allocated at each one's first write, so a server's reply spends
  nothing of its own; but a client's sends are not yet capacity its receiver
  chose and was charged for at accept.
- **Floor deficits are the machine's, not each floor job's.** A floor's frames
  and whole superframes are held again only as they come back free and whole
  (`kernel/pure/frames.rs`), and what is short is pooled: one floor job's
  pinned or scattered give-back can refuse another floor job inside its own
  floor. Owner: the memory track. Exit: a law where one floor job's deficit
  never refuses another's use inside its floor.
- **A USB keyboard is not protected.** usbd drives disks as well as keyboards,
  so it is a mixed service and has no floor; the i8042's ps2server has one.
  Owner: the USB track. Exit: USB input protected as the review of the input
  path above decides; a driver split only to assign a floor is not
  presumed.
- **A `drives` process refused its start for memory is not retried on the
  memory event**, only at the next inventory read; a service is. Owner: the
  supervisor track. Exit: a `drives` start refused for memory waits on its
  job's readiness, as a service's restart does.
- **Kill draws on the kernel's kept superframes.** A kill and the teardown it
  starts allocate transient heap, which the kept superframes cover rather
  than an allocation-free path. Owner: the memory track. Exit: kill and
  teardown allocate nothing.
- **`account_info` reports no resident pages, retained objects or live
  processes**, which the memory design lists. Owner: the memory track. Exit:
  the three in `AccountInfo`, read without the process lock.

**Exit:** a person can see which job holds the memory and stop it while every
frame is held, through a control drawn and served from capacity committed
before the exhaustion; and a connection's receive capacity is charged to its
receiver at accept. The guest test is `memory_floor`'s, its system holder
replaced by the control.

**The class statement, which is what predicts the next instance.** *A client's
request is an allocation request, and every one of them needs an owner who can
say no.* Three instances were filed under it — the compositor's windows,
netstack's piped connections, and `SYS_CONNECT` pinning 4 MiB into an
unbounded pending queue — and all three have a bound *and* a caller that hears
the refusal: `toyos_desktop::max_windows` and netstack's `places_for` each
divide an eighth of physical memory by what one unit costs and refuse past it,
and a pipe takes memory for its bytes as it holds them rather than at
`create`. Their
memory is counted to whichever job made it: a client's own connection is
its job's, but a pipe or an event a server makes and hands out
(fileserver's stream pipes, netstack's bells) is the server's, which the
server bounds. With no
per-job limit a cap is still the only thing between one client and the
machine. The file servers' block caches hold 16 MiB each, and their pagers'
clean pages up to an eighth of physical memory between them, and no other job gets either back
(`issues/the-file-servers-clean-blocks-are-given-back-to-no-other-job.md`,
`issues/a-pager-holds-a-fixed-capacity-until-the-memory-service-reclaims-its-clean-pages.md`).

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling: isolation
findings are security-adjacent and are promoted, never folded). One job
starving every other of memory is a denial of service across the isolation
boundary that needs no privilege and nothing crafted.
