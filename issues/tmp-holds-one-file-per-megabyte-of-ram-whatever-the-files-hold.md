---
status: open
kind: defect
opened: 2026-11-11
severity: [security]
---

# `/tmp` holds one file per megabyte of RAM, whatever the files hold

The file server's MEMORY role (`/tmp`) sizes its pager as DATA's is sized:
`pager` in `system/fileserver/src/main.rs` gives it capacity/16 objects,
clamped to between 1,024 and 65,536. Its capacity is an eighth of RAM, less
LOG's and BOOT's pages, then halved between DATA and MEMORY. Each file is one
anonymous object (`system/fileserver/src/pages.rs`). Unlike a DATA file nobody
holds, an anonymous object is never dropped to make room. So the object count
is a hard cap on how many files `/tmp` holds, and it does not depend on what
the files contain. Past the cap, every create is refused `NoSpace`
(`fileserver: a paged object refused: NoRoom`), empty files included. Any
client can therefore take every `/tmp` file every other client needs: in the
guest row's run, one member's leftovers failed the 14 members after it that
write `/tmp`.

On a 4 GiB guest the kernel manages 3,905,302,528 bytes (953,443 frames), and
the arithmetic gives:

- capacity: (953,443 / 8 − 2 × 4,096) / 2 = 55,494 pages
- objects: 55,494 / 16 = 3,468

That is one `/tmp` file per 1,126,096 bytes of the machine's memory. An empty
file holds no page and needs nothing near that. `readdir_bound`, making 6,000
empty files, was refused at index 3464 with "create file 3464 of 6000:
StorageFull" (a development run of the `testcases_members_exit_0` row at
`edb33219f`, x86-64 under TCG, `-m 4G`):
https://github.com/ToyOSOrg/evidence/tree/133bf3ff204ad9c45549c19e7cc7b1bc773fb8e8/pr-1271/tmp-cap

A service is to bound what its clients make it grow by what that growth costs
the client that asked. A ratio of RAM fixed when the pager is made bounds
neither.
`issues/a-pager-holds-a-fixed-capacity-until-the-memory-service-reclaims-its-clean-pages.md`
records the pages' interim. This issue is the object count's effect on a volume
whose objects cannot be dropped.

**Owner**: the process-memory track. The sizing is the file server's (`pager`
in `system/fileserver/src/main.rs`); what the kernel charges a pager per
object (`kernel/src/object/paged.rs`) bounds the fix.

**Exit**: an empty `/tmp` file costs a bounded few hundred bytes, charged to
whoever made it, and the number of `/tmp` files is bounded by that charge
rather than by a share of RAM fixed when the pager is made. The cheapest proof
is a host module test of the file server over its `Pager` trait, whose fake
(`system/fileserver/src/pages/tests.rs`) stands for the kernel: more than 3,468
empty files are made and removed on a memory volume sized as a 4 GiB machine's. A guest test is needed only for whatever the kernel's per-object
charge must show.
