---
status: open
kind: defect
opened: 2026-11-01
---

# The wlan handshakes' readings of 802.11 are unchecked against its text

`system/wlan/handshake` cites IEEE Std 802.11-2020 and IEEE Std 802.1X-2020 by
clause, and its rules rest on readings of them. They were written without the
text at hand. The recorded handshakes (`tests/capture.rs`) confirm the frames
and keys a correct authenticator and supplicant exchange, but not the
citations or the refusals, which no capture exercises.

- **Owner:** the orchestrator, who holds the text, as for W1's check (#924,
  issuecomment-6173453493).
- **Exit:** each row below is confirmed or corrected against the text, with
  page numbers, in a comment on the pull request that lands the fixes. Then
  this file is deleted. If the check is made against 802.11-2024, the sites
  are moved to its clause numbers.

| # | Site | Reading to check |
|---|---|---|
| 1 | `eapol.rs` | EAPOL Packet Type 3 is EAPOL-Key, Protocol Versions 1 to 3 exist (802.1X-2020 §11.3) |
| 2 | `eapol.rs` | Descriptor Type 2 is the IEEE 802.11 key descriptor; its fixed fields and their order, with AKM 2's 16-octet MIC (§12.7.2) |
| 3 | `eapol.rs` | Key Information bits: version 0–2, Key Type 3, Install 6, Ack 7, MIC 8, Secure 9, Error 10, Request 11, Encrypted Key Data 12 (§12.7.2) |
| 4 | `eapol.rs` | Key Descriptor Version 2 is HMAC-SHA1-128 and AES key wrap, and is what AKM 2 with CCMP-128 uses (§12.7.2) |
| 5 | `eapol.rs` | Key Length is 0 in messages 2 and 4 and group message 2 (§12.7.6.3, §12.7.6.5, §12.7.7.3) |
| 6 | `eapol.rs` | Octets past the Packet Body Length are not the PDU's |
| 7 | `kde.rs` | Key Data padding is 0xDD then zeros (§12.7.2) |
| 8 | `kde.rs` | KDE selectors under 00-0F-AC: GTK 1, IGTK 9; the GTK KDE's Key ID bits 0–1 and reserved octet; the IGTK KDE's Key ID, 6-octet IPN and key |
| 9 | `kde.rs` | A receiver skips the elements and KDEs it does not know (§12.7.2) |
| 10 | `supplicant.rs` | PTK = PRF-384(PMK, "Pairwise key expansion", Min(AA,SPA) ‖ Max(AA,SPA) ‖ Min(ANonce,SNonce) ‖ Max(ANonce,SNonce)), split KCK, KEK, TK (§12.7.1.3) |
| 11 | `supplicant.rs` | Message 3's RSNE must be identical to the Beacon's or Probe Response's (§12.7.6.4), and so must its RSNXE, including when either is absent |
| 12 | `supplicant.rs` | Message 2 carries the station's RSNE and, if it sent one, its RSNXE |
| 13 | `supplicant.rs` | A second RSNE in message 3 offers a pairwise cipher; refusing it is a choice, not the text |
| 14 | `supplicant.rs` | Replay rule: a counter not above the last one verified is discarded, message 1 included |
| 15 | `supplicant.rs` | GTK key ID 0 is never a group key; an IGTK's is 4 or 5 |
| 16 | `supplicant.rs` | The RSNE defaults: CCMP-128 for the ciphers, 802.1X for the AKM, BIP-CMAC-128 for group management |
| 17 | `supplicant.rs` | CCMP's PN is Key RSC's first six octets, least significant first |
| 18 | `supplicant.rs` | Every frame an authenticator sends has Key Ack set and Request and Error clear |
| 19 | `supplicant.rs` | Message 1: Pairwise, no MIC, Install clear, Encrypted Key Data clear |
| 20 | `supplicant.rs` | Message 3: Pairwise, MIC, Install, Secure and Encrypted Key Data set |
| 21 | `supplicant.rs` | Group message 1: Key Type group, MIC, Secure and Encrypted Key Data set, Install clear |
| 22 | `supplicant.rs` | Key Length is 16 for CCMP-128 in messages 1 and 3, and need not be checked in group message 1 |
| 23 | `supplicant.rs` | Key RSC, and the IGTK KDE's IPN, are the last packet number the authenticator sent under the key, which the receive replay check starts from (`GroupKey::rsc`, `IntegrityKey::ipn`) |
| 24 | `supplicant.rs` | PMF is negotiated when both sides set MFPC; either side's MFPR without the other's MFPC refuses the association |
