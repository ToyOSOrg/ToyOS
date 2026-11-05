# Memory

## Pages and mappings

Process memory uses 4 KiB pages; 2 MiB pages remain where they pay, for large
mappings, DMA grants, the IOMMU and the kernel's own mappings. A large virtual
mapping never needs contiguous physical memory.

Address reservation, commitment and population are separate. Reserving
address space costs nothing; committing backing is strict; pages are populated
lazily. A successful anonymous commitment covers every later ordinary fault on
it. When capacity runs out, new commitments are refused; nothing is killed to
make room.

Each thread's stack capacity is committed before the thread starts, populated
lazily and guarded, with caller-chosen sizes and ordinary creation failure.

Programs may generate executable code without a prompt, while write and
execute stay separate and executable pages are shared safely. Shared memory may
be installed at a requested address in a child before it starts; conflicts are
refused. There is no address-space cloning.

## Who owns what

```text
   kernel           faults, page tables, accounting, reverse records,
                    paging mechanisms; no policy, no filesystem, no crypto
   memory service   reclaim policy, pressure figures, swap placement,
                    sealing and opening swapped pages
   file servers     file identity, contents, access and writeback
```

One authoritative page cache serves both file I/O and file mappings. The kernel
supplies the generic paging mechanism; file servers supply and write back file
pages, and the memory service decides what to reclaim. Mapped files may be
larger than RAM, partially mapped and coherently shared and written; their
length is never committed in RAM. Clean pages are reclaimed; dirty pages are
written back first, without losing concurrent writes. Dirty data growth is
bounded, and writeback starts ahead of pressure.

Clean executable pages may be dropped and reloaded from the same executable
version. An update or uninstall never substitutes different contents for a
running program.

## Accounting

Every allocation has one owner, the job it is charged to. Sharing keeps the
charge with its owner, and the record survives the creator until the backing is
released; a copy is a new charge. Each mapper pays for its own page tables and
mapping metadata. Shared immutable file backing is counted once, in a shared
account. Services own their private state and bound what clients can make them
grow; client-owned bulk buffers are preferred.

Apps share memory without fixed per-app quotas. A reserve protects system
inspection and stop controls. A program may lock a modest, bounded amount of
memory; more needs explicit resource authority, and helpers do not multiply an
allowance.

## Swap

Anonymous memory may leave RAM for an encrypted, authenticated swap partition,
preallocated on internal storage, with keys that live for one boot. Commitment
counts usable RAM plus reserved swap, never hoped-for compression or growth.
Swap adds capacity, not responsiveness.

- Swapped memory may make a load, a store or a copying system call wait on
  storage. The wait advances with storage progress and is reported when
  overdue; a timeout never turns it into an error.
- Swapped memory survives a crash and authorised restart of the memory
  service. Swap does not survive a reboot.
- A page is lost only when a running memory service establishes the loss: a
  failed read after the driver's recovery, a failed authentication or a device
  that is gone. Lost anonymous memory ends the process at its next touch, with
  a diagnostic; contents are never invented. Pressure, slowness or a hang never
  justifies ending a process.
- Executable and read-only image pages never go to swap.
- One program's swapped memory delays only that program.

## Pressure

Reclaim balances access history and refault feedback, and reclaims and writes
back in advance. The paging, storage and recovery paths it depends on stay
resident and prepared. Pressure is reported and the user keeps control; nothing
suspends or kills workloads automatically.

## Allocators

The heap allocators share machinery where useful, with separate kernel and
userland policies, size classes, bounded local caches, kernel metadata kept
apart from data and efficient cross-thread frees. Unused backing returns under
pressure, and ordinary Rust and C allocation contracts hold without Rust
depending on libc.
