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

Their commits still resolve there by hash. Over the 12 tips E10 prints past
`wt/toyos-survey`, `git rev-list <tips> --not --remotes=origin` prints 52
commits. No ref on `origin` holds any of them, pull requests' refs included,
`origin` has no fork, and GitHub's commit API for `origin` answers all 52
(E15,
https://github.com/ToyOSOrg/evidence/blob/6196aad98a0bf5eff937a25b9dad4ec10e59376f/e15-demo-commits-on-origin.log).
The API's answer for each of the 12 tips carries the model name (E12 §2).

Three logs on `ToyOSOrg/evidence`, whose branches only grow, print hashes
that resolve there:

- E10 and E11 print the 12 tips in full;
- E13 §2 quotes a build by its hash, abbreviated to nine hex digits (its
  line 80). Nine of the tips hold that commit, and it is one of the 52.

What the demo's commits carry:

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

Owner: the orchestrator. The owner, whose call the demo's branches were,
ruled to leave the commits on GitHub: "leave it". Nothing asks GitHub to
remove them, so the exit waits for GitHub's garbage collection of commits no
ref holds.

**Exit**: no ref on `origin` carries the model name, which E12 §1 already
reads; and GitHub's commit API for `origin` answers none of the commits
`git rev-list <tips> --not --remotes=origin` prints over those 12 tips, where
E15 reads 52 of 52. The local branches are kept for readers' scouts, and
every reader's scout stays off `origin` (#876 §7).
