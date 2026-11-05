# Boot and updates

## The handover

```text
  firmware
     └─> loader       makes every UEFI call; reads the chosen slot into
         │            memory and checks it against the signed header
         └─> kernel   keeps those images read-only; starts one program
             └─> supervisor
                 ├─> SYSTEM file server, serving ROOT from memory
                 └─> everything else, from the system manifest
```

The loader does only what must happen before the handover. Every UEFI call
ToyOS makes is the loader's, before it exits boot services; the kernel calls no
firmware service. The loader uses its own UEFI bindings.

The loader reads the chosen slot's kernel, boot parameters, the supervisor's
and the system file server's executables and ROOT, the system image, into
memory through the firmware's block I/O, and checks them against the slot's
signed header. It hands the kernel a typed description of what it loaded and
why it chose that slot. No storage driver is needed to boot: the kernel keeps
those pages read-only, starts the supervisor from its image and hands it the
other two, and the supervisor starts the SYSTEM file server serving ROOT from
that memory before it reads a single file.

The hardware clock keeps UTC.

## Signed images

An image's header names its security version and the SHA-256 of each part, the
kernel, its parameters, the two boot programs and ROOT, by role and length, and
is signed with Ed25519 in the OpenSSH signature format, so standard SSH tools
sign and verify it. The signature covers hashes, never bytes.

## Two slots

The disk carries two slots, each a boot volume and a ROOT, named by a slot
table. Each slot carries a priority, the tries it has left and a flag saying a
boot of it was good.

- An install writes the slot the machine is not running, untried, with a few
  tries and a priority above the running slot's.
- A boot takes the highest-priority slot that verifies and is either good or
  has tries left; a try is spent before the handover.
- A slot is refused, and the other one tried, when its signature fails, another
  key signed it, a byte is wrong, its security version is below the floor, or
  the image itself fails to start.
- A slot becomes good once the services the image names as essential report
  they are serving.
- When no slot can boot, the loader says so on the panel and in its log, and
  powers off.

Updating is an ordinary program: a signed image on standard input, written to
the idle slot and marked to boot next. The signature is its whole authority,
and how the bytes arrived does not matter. Updates are pulled from a release
server, as content-addressed chunks of which only those the idle slot lacks
cross the network. User data lives on DATA, which no slot includes, so an
update never touches it; apps update separately through the package manager.

## Rollback

The loader keeps an anti-rollback floor per signing key, in firmware storage
that nothing after the handover can reach. The floor counts a signed security
version, raised only by a release that fixes a security hole, and it rises when
a good slot boots.

## The verified system

- An image-based, read-only system: `/system` is the signed ROOT and nothing
  else, and nothing the machine runs is outside an image or a package.
- A verified chain: UEFI Secure Boot with the owner's key over the loader, the
  loader over every part of the slot it boots, and any failing link refused by
  name.
- Anti-rollback that holds against someone with the machine in hand.
