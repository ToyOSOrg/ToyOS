---
status: open
kind: defect
opened: 2026-10-12
---

# The Wi-Fi demo's commits still resolve on origin

The Wi-Fi demo ran on the AMD bench and never lands. Its branches are gone
from `origin`, a public repository. Read at `origin/main` `ab655a115` (E12 §1,
https://github.com/ToyOSOrg/evidence/blob/2eb315326d1b4d0363b4c918da8dd2bfaabd1234/e12-demo-on-origin.log):

- no ref on `origin` carries the AMD bench's model name;
- the two refs that still hold the demo's first commit, `a8e1c96`, touch no
  Wi-Fi-named path and carry no line with that name past `origin/main`.

At `c0cdddf4e` E10 had counted 11 branches holding it that touched a
Wi-Fi-named path
(https://github.com/ToyOSOrg/evidence/blob/5d27b8209199228ccc1e133b69ad4c3758f18510/e10-demo-branches-on-origin.log).

Their commits still resolve there by hash. GitHub's commit API for `origin`
answers each of the 12 tips E10 prints past `wt/toyos-survey`, and each
answer carries the model name (E12 §2). E10 and E11 print those tips in full
on `ToyOSOrg/evidence`, whose branches only grow.

What they carry:

- the model name, which E10 to E13 mask: in commit subjects (E13 §2 quotes
  two,
  https://github.com/ToyOSOrg/evidence/blob/2eb315326d1b4d0363b4c918da8dd2bfaabd1234/e13-amd-ax200-bar.log),
  and in the heading of the firmware entry they add to `NOTICE` (E11,
  https://github.com/ToyOSOrg/evidence/blob/de051da9eaff0cfae26a16695b478f0d6f46648d/e11-demo-notice-entries.log);
- a driver written against a reference rather than from a specification: the
  message of E10 line 52's tip says its `netstack/src/wifi.rs` was "written
  against OpenBSD's ISC-licensed if_iwx (22000 family)" (E12 §3), and no
  demo branch adds a `NOTICE` line naming a reference (E10). The owner's
  ruling admits a reference only as clean-room information, which takes no
  notice (#876 §0); code written against one is not that, and carries none
  [I].

Owner: the orchestrator, who takes it to the owner, whose call the demo's
branches were.

**Exit**: no ref on `origin` carries the model name, which E12 §1 already
reads; and GitHub's commit API for `origin` answers none of the 12 tips E10
prints past `wt/toyos-survey`, where E12 §2 reads 12 of 12. The local branches
are kept for readers' scouts, and every reader's scout stays off `origin`
(#876 §7).
