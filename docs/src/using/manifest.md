# The system manifest

The signed image carries one system manifest: the whole statement of what the
system runs and what each program holds. The supervisor builds every program's
starting authority from it and from nothing else, and the build refuses a
manifest that contradicts itself, such as two programs claiming one device.

```text
   manifest row ──> supervisor ──> spawn with:  connectors (services it receives)
                                                acceptors (services it serves)
                                                view (directories)
                                                device claims
                                                narrowed system rights
```

## What a row grants

- **Services**: the ports it serves and the ports it may connect to.
- **Devices**: the devices it claims at start, or the kinds of device it drives,
  one process per matching device.
- **Launch**: the programs it may launch, the only way to launch one.
- **Login**: whether it is a login authority that may ask for a session.
- **System rights**: the narrowed rights it holds on the system capability.

Which programs start at boot is a list without order, since every port exists
first. Every installed app receives one shared set of services the manifest
names; a package carries no row of its own that could grant it devices, system
rights or directories. Only the image grants privileges
([Trust](../architecture/trust.md)).
