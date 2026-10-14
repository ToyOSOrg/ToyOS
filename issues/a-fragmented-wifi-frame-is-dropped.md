---
status: open
kind: defect
opened: 2026-10-14
---

# A fragmented Wi-Fi frame is dropped

`toyos-ieee80211` refuses every fragment by name (`Refused::Fragmented`,
`lib/ieee80211/src/frame.rs`): a frame whose More Fragments bit is set or whose
Fragment Number is not 0. It does not reassemble the MSDU or MMPDU.

**The refusal is the right default under the trust model.** Frames are
untrusted input, and reassembly is where Vanhoef's fragment-and-forge attacks
live ("Fragment and Forge: Breaking Wi-Fi Through Frame Aggregation and
Fragmentation", USENIX Security 2021): fragments of different keys, of a frame
sent before a rekey, or a plaintext fragment mixed into an encrypted frame. A
reassembly buffer that does not exist cannot mix them.

**The cost is real.** Every MSDU that an access point or a peer fragments is
lost. 802.11 lets a transmitter fragment any unicast MSDU or MMPDU above its
fragmentation threshold (IEEE 802.11-2020). An AP configured with a
threshold below its MTU drops part of the station's traffic, retry by retry,
with nothing upstream told why.

- **Owner:** the orchestrator, for the Wi-Fi track (`issues/there-is-no-wifi.md`).
- **Evidence:** the refusal and its test, `every_refusal_has_a_hostile_input_that_produces_it` in `lib/ieee80211/tests/refusals.rs`.
- **Exit:** reassembly that holds the fragment-and-forge defences, each refusal tested by host tests with the paper's attack frames:
  - fragments reassembled only under one key and one packet-number run, with consecutive packet numbers;
  - the buffer cleared on a rekey or reassociation;
  - a plaintext fragment never joined to protected ones;
  - a bounded buffer per sender and TID.
