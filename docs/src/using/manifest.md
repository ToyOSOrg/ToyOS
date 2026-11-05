# The system manifest

The signed image carries one system manifest. It is the whole statement of what
the system runs and what each program holds; the supervisor builds every
program's starting authority from it and from nothing else. The build refuses a
manifest that contradicts itself, such as two programs claiming one device or a
row naming a service no row serves.

```text
   manifest row ──> supervisor ──> spawn with:  namespace (services it receives)
                                                acceptors (services it serves)
                                                view (directories)
                                                device claims
                                                narrowed system rights
                                                HOME and state directory
```

## What a row says

| A row may say | Meaning |
|---|---|
| it serves a service | the supervisor creates the port and hands the program its acceptor |
| it receives a service | its namespace holds a connector to that port |
| it claims devices | the named device classes or functions, moved to it at start |
| it drives devices | one process of it per matching device function, at boot or on arrival |
| it is a service | it runs as a system service, with its home in its own state directory |
| it keeps state | it gets a private state directory beside the session user's home |
| it may restart | the supervisor restarts it, within a bounded number of attempts |
| it starts programs | the programs it may launch; the only way to launch one |
| it is a login authority | it may ask for a login session |
| system rights | the narrowed rights on the system capability it holds |
| a memory floor | capacity protected for a control path's job |

The manifest also names which programs start at boot (the order means nothing,
since every port exists first) and the single set of services every installed
app receives. An installed package carries no row of its own that could grant
it devices, system rights or directories: what every app holds is the image's
answer, and what a particular app holds beyond it is the user's grant.

## Privileges only the image grants

A device claim, a system right and every privilege beyond an ordinary claim are
granted only by the signed image's manifest, to programs that image ships. See
[Trust](../architecture/trust.md).
