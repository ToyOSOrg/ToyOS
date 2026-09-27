---
status: open
kind: defect
opened: 2026-09-26
---

# OVMF's licence record names no OpenSSL

`NOTICE`'s `ovmf/*.fd` section and its three `src/licence.rs` rows say
`BSD-2-Clause-Patent`. EDK II's platforms link its bundled OpenSSL into the
firmware through `CryptoPkg` (`BaseCryptLib`, and `TlsLib` when
`NETWORK_TLS_ENABLE` is set), and OpenSSL 3 is Apache-2.0. For the AArch64
firmware this is established and recorded (PR #524: edk2-stable202408's
ArmVirtQemu links OpenSSL 3.0.9 whether TLS is on or off, and QEMU builds it
with TLS on). For OVMF nothing is: the section itself says the files came
with no version record or build recipe, so which CryptoPkg libraries they
link is unknown, and the record claims a single licence nobody has checked.

**Exit condition**: the OVMF images' crypto content is read out of the
binaries or their upstream build, and the section and rows state every
licence it carries, or the images are replaced by ones whose build is known
(the section's own open question to the owner).
