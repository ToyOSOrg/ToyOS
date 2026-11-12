# Memory

## Pages and mappings

- Process memory uses 4 KiB pages; 2 MiB pages remain where they pay: large
  mappings, DMA grants, the IOMMU and the kernel's own mappings.
- A large virtual mapping never needs contiguous physical memory.
- Reservation, commitment and population are separate. Reserving address space
  costs nothing, committing backing is strict, and pages are populated lazily.
- A successful anonymous commitment covers every later ordinary fault on it.
  When capacity runs out, new commitments are refused; nothing is killed to
  make room.
- Each thread's stack capacity is committed before the thread starts, populated
  lazily and guarded, with caller-chosen sizes and ordinary creation failure.
- Programs may generate executable code without a prompt; write and execute
  stay separate and executable pages are shared safely: shared memory is
  executed only where nobody can write it.
- A range of a memory object is mapped at a requested address, in the caller
  or in a child before it starts; conflicts in a child are refused. There is
  no address-space cloning.
- A program's read-only segments are the pages of its image wherever nobody
  can write the image, held once however many processes run it. Its writable
  segments are private copies, taken as they are touched.

## The kernel's own memory

- Every stack the kernel runs on sits above memory that is not mapped, so
  running off one stops the machine with a report naming the stack instead of
  writing memory another owner holds.
- A thread's kernel stack is frames its job holds, mapped in a kernel address
  range of its own whose gaps are never mapped. The frames go back when the
  thread ends; the address is reused only once no CPU can still translate it.
- That range's page tables are the kernel's own and are kept: one for each
  eight stacks ever in use at once and a fixed few more, about 2 MiB for a
  process at its thread limit. A thread that cannot get one is refused, as
  is one past the fixed number of stacks the range holds at once.
- A CPU's own stacks are set aside when it is brought up and kept for the
  machine's life.

## Who owns what

```text
   kernel           faults, page tables, accounting, reverse records,
                    paging mechanisms; no policy, no filesystem, no crypto
   memory service   reclaim policy, pressure figures, swap placement,
                    sealing and opening swapped pages
   file servers     file identity, contents, access and writeback
```

- One authoritative page cache serves both file I/O and file mappings. A
  file's pages are a paged memory object its file server supplies; an open
  file's holders read it, write it and read its size and times without a
  request. The server's own cache holds only volume structures.
- A holder writes without a request against credit: pages it may turn
  dirty, for whose write-back the server holds room on the volume. A write
  the credit does not cover, longer than 256 KiB, that meets a page the
  server must supply first, or that meets another write under way goes to
  the server whole, so a full volume is answered at the write and never at a
  later write-back. While holders may write, the object's size and
  modification time are the file's, and the server writes them back with
  its pages.
- Writes to one file go one at a time, as POSIX requires: each, a holder's,
  the server's or an append, lands whole before or after every other, and
  an append lands at the end the last write left. A read, and a statement of
  the file's size and time, sees each write whole or not at all; a holder's
  read that keeps meeting writes goes to the server, whose reads hold the
  writes off. A holder's write never waits for another, and nothing a holder
  does keeps the server's writes waiting.
- Mapped files may be larger than RAM, partially mapped, and coherently shared
  and written; their length is never committed in RAM.
- A file mapping maps the cache's own pages, so a mapper sees a write as it
  lands, and a private mapping shares them until it is written. A mapping
  holds its file as an open handle does: a file mapped and then closed or
  deleted stays until its last mapping goes. A page leaves the cache only once
  no mapping reaches it.
- A wait for a file's page behaves like one for swapped memory: it advances
  with its file server's progress, is reported when overdue and never becomes
  an error by timeout. The server failing what it cannot read, and its device
  waits' own timeouts, bound it.
- A touch of a mapped page its file server failed, or of a file whose server
  ended, ends the program with a diagnostic naming the file's page and the
  cause; a copying system call answers an I/O error instead. A touch of a
  mapped page wholly past the file's end ends the program the same way, as
  POSIX's `SIGBUS`, and a copy from one is refused as a bad address. A touched
  unwritten page of a file held in RAM becomes resident within its server's
  capacity and is refused once that is full.
- Clean pages are reclaimed; dirty pages are written back first, without losing
  concurrent writes. Dirty data growth is bounded, and writeback starts ahead
  of pressure.
- Clean executable pages may be dropped and reloaded from the same executable
  version. An update or uninstall never substitutes different contents for a
  running program.

## Accounting

- Every allocation has one owner, the job it is charged to. Sharing keeps the
  charge with its owner, and the record outlives the creator until the backing
  is released; a copy is a new charge.
- Each mapper pays for its own page tables and mapping metadata.
- Shared immutable file backing is counted once, in a shared account.
- Services own their private state and bound what clients can make them grow;
  client-owned bulk buffers are preferred.
- Apps share memory without fixed per-app quotas. A reserve protects system
  inspection and stop controls.
- A program may lock a modest, bounded amount of memory; more needs explicit
  resource authority, and helpers do not multiply an allowance.

## Swap

Anonymous memory may leave RAM for an encrypted, authenticated swap partition,
preallocated on internal storage, with keys that live for one boot.

- Commitment counts usable RAM plus reserved swap, never hoped-for compression
  or growth. Swap adds capacity, not responsiveness.
- Swapped memory behaves like RAM: it may make a load, a store or a copying
  system call wait on storage. The wait advances with storage progress and is reported when
  overdue; a timeout never turns it into an error, and a non-blocking
  descriptor does not cover the caller's own buffer.
- A page is lost only when a running memory service establishes the loss: a
  failed read after the driver's recovery, a failed authentication or a device
  that is gone.
  - Lost anonymous memory ends its process at the next touch, with a
    diagnostic; contents are never invented.
  - A buffer inside a file mapping answers an I/O error instead.
  - A futex wait on a swapped or lost word returns as a spurious wake.
- With the memory service stopped or past its restart budget, nothing fails:
  its faults wait, reported as such, until an authorised restart. The user may
  stop any waiting program.
- If backing shrinks below commitment, new commitments are refused and a fault
  past the remaining backing waits, reported with the deficit.
- Swapped memory survives a crash and authorised restart of the memory
  service. Swap does not survive a reboot.
- Executable and read-only image pages never go to swap.
- One program's swapped memory delays only that program; no service waits on
  one client's memory in a path that serves others.

## Pressure

- Reclaim balances access history and refault feedback, and reclaims and
  writes back in advance.
- The paging, storage and recovery paths it depends on stay resident and
  prepared.
- Pressure is reported and the user keeps control. Pressure, slowness or a hang
  never justifies ending, suspending or killing a workload.

## Allocators

The heap allocators share machinery where useful, with separate kernel and
userland policies, size classes, bounded local caches, kernel metadata kept
apart from data and efficient cross-thread frees. Unused backing returns under
pressure, and ordinary Rust and C allocation contracts hold without Rust
depending on libc.
