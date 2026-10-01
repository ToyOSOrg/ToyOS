---
status: open
kind: tooling
opened: 2026-10-01
---

# A warm host run keeps what a registry proc macro expanded from a file it never named

A registry proc macro is compiled once and runs inside every compile of the
crate that expands it. One that reads a file without telling rustc, the way
`wayland-scanner`'s `generate_client_code!` opens its XML, reads it again only
when cargo recompiles the expanding crate. A warm `host` run
(`src/cicache.rs`) recompiles a path crate only when its own package changed,
so when that file sits outside the expanding crate's package and changes
alone, the warm run keeps the old expansion where a cold run makes a new one.
Cargo never dates a registry source, so nothing dates the macro's package.

Of the proc macros in the lockfiles of the five workspaces the host job builds,
`wayland-scanner` alone reads a file it does not name, and no tracked source
names it.

Done when a warm read serves what a cold build serves for a path crate that
expands a registry proc macro reading another package's file, with a test.
