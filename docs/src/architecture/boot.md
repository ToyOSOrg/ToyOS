# Boot and updates

```text
  firmware
     └─> loader       makes every UEFI call; reads the chosen slot into
         │            memory and checks it against the signed header
         └─> kernel   keeps those images read-only; starts one program
             └─> supervisor
                 ├─> SYSTEM file server, serving ROOT from memory
                 └─> everything else, from the system manifest
```

## The loader

- It does only what must happen before the handover.
- Every UEFI call ToyOS makes is the loader's, before it exits boot services,
  through its own UEFI bindings; the kernel calls no firmware service.
- It loads the slot's kernel, boot parameters, the two boot programs and ROOT
  through the firmware's block I/O, so no storage driver is needed to boot.
- It hands the kernel a typed description of what it loaded and which slot it
  chose.
- Where it loaded from is not the kernel's to know: the partitions it names,
  by role, and the firmware's device paths for them travel as a record the kernel
  keeps read-only and hands the supervisor unread. The supervisor validates it
  once and refuses a record it cannot read.
- On every boot it asks the firmware to overwrite memory before the next one
  (the TCG memory-overwrite request), so on firmware that defines and honours
  the request a reset leaves nothing of this boot in RAM for whoever boots
  next.
- The hardware clock keeps UTC.

## Signed images

- An image's header names its security version and the SHA-256 of each part,
  by role and length, and is signed with Ed25519 in the OpenSSH signature
  format, so standard SSH tools sign and verify it.
- The signature covers hashes, never bytes.
- A slot whose image fails verification is refused by name: an unsigned image,
  another key's, a wrong byte, or a security version below the floor.

## Slots and updates

- The disk carries two slots, each a boot volume and a ROOT, named by a slot
  table. An update is written to the slot the machine is not running.
- Updates fix forward. The loader boots the slot marked to boot next and no
  other; a slot that is refused or fails is reported, never replaced by the
  previous one automatically. After a refusal the machine reports it and
  powers off; it does not reset into anything else. The remedy is a newer
  image.
- Updating is an ordinary program: a signed image on standard input, written to
  the idle slot and marked to boot next. The signature is its whole authority,
  and how the bytes arrived does not matter.
- Updates are pulled from a release server, as content-addressed chunks of
  which only those the idle slot lacks cross the network.
- User data lives on DATA, which no slot includes; apps update separately
  through the package manager.

## The verified system

- `/system` is the signed ROOT and nothing else; nothing the machine runs is
  outside an image or a package.
- UEFI Secure Boot with the owner's key covers the loader, the loader covers
  every part of the slot it boots, and a failing link is refused by name.
- An anti-rollback floor per signing key counts the signed security version,
  raised only by a release that fixes a security hole, and holds even against
  someone with the machine in hand. A boot proves its image by stopping
  cleanly: once the stop has synced its files, and before its drivers stop,
  the log keeper marks the log partition, which only the log keeper and the
  supervisor may write, and only the next loader pass that finds that mark
  raises the floor to the image's version.
