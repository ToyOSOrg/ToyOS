---
status: open
kind: defect
opened: 2026-10-12
---

# The Wi-Fi demo's branches are on origin

The Wi-Fi demo ran on the AMD bench, and its branches never land. Eleven of
them are on `origin`, a public repository, and touch a Wi-Fi-named path.
Read at `origin/main` `c0cdddf4e`:

- 13 branches on `origin` hold the demo's first commit, `a8e1c96`, and 11 of
  them touch a Wi-Fi-named path past `origin/main` (E10,
  https://github.com/ToyOSOrg/evidence/blob/5d27b8209199228ccc1e133b69ad4c3758f18510/e10-demo-branches-on-origin.log);
- each of the 11 adds one `NOTICE` entry, the AX200 firmware's, and none
  adds a line naming a reference (E10, and E11,
  https://github.com/ToyOSOrg/evidence/blob/de051da9eaff0cfae26a16695b478f0d6f46648d/e11-demo-notice-entries.log).

#876 §8 records them as written by an agent that read the references, so
they carry no notice of their source [I]. The owner ruled the references
"clean room information" with "no notice for it" (#876 §0). The branches'
names, and the heading of the firmware entry they add, also carry the AMD
bench's model name, which E10 and E11 mask.

Owner: the orchestrator.

**Exit**: E10's count of branches on `origin` that hold `a8e1c96` and touch a
Wi-Fi-named path reads 0. The local branches are kept for readers' scouts,
and every reader's scout stays off `origin` (#876 §7).
