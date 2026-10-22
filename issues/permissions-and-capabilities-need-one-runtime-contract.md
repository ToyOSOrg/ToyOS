---
status: open
kind: track
opened: 2026-10-16
---

# Permissions and capabilities need one runtime contract

## Settled boundaries

One userland permission service owns remembered decisions and consent. Supervisor
owns process/session lifecycle and trusted launch identity. Resource services
(fileserver, netstack, soundserver, compositor) authorize fresh grants through
that service and enforce issued capabilities locally. No per-operation permission
round trip, per-category daemon or kernel permission-policy engine.

A capability carries authority; a name, app-supplied identity or connector badge
does not establish the identity of its current holder. Transfers narrow or retain
rights, never widen them. Ordinary helpers receive passed/inherited authority;
selecting an installed executable does not activate its saved approvals.
Independent app launch needs explicit launch authority and establishes its trusted
identity. The same rules apply to shells, editors, agents and other programs.

## Consent and lifetime

- Apps may request ordinary resources at runtime without a manifest permission
  list. Trusted file selection grants a location, not automatic folder access or
  rename tracking. Failed arbitrary path lookup does not itself summon consent.
- Network first use can prompt. Separate outgoing and unsolicited incoming
  permission, per app, including local connections. No per-port consent; show
  listeners and refused attempts. Replies to outgoing traffic need no incoming
  approval; UDP classification still needs a concrete protocol design.
- Without an approval interface, refuse promptly. Explicit permission commands
  support headless/SSH use; no automatic terminal-prompt coordination.
- Verified same-source updates preserve approvals and denials. Unverified
  replacements do not inherit them; uninstall forgets decisions even if data stays.
  Unpackaged programs receive direct approvals for that run only.
- Any program may pass transferable file/socket access without another prompt.
  Show provenance. A recipient denied direct network access may use delegated
  access. Copies retain the original grant's scope, not independent saved approval.
- Removing permission blocks future direct grants only. Existing broad grants may
  still permit fresh operations/connections and further transfer. Do not implement
  live revocation or automatic app termination for a changed saved decision.
- Ordinary resource-session lifetimes still apply: stopping screen sharing ends
  its access. Existing capabilities survive permission-service failure; fresh
  authorization cannot succeed while unavailable. Parent restart is bounded.

## Work and exit

Design protected installation/launch identity, authenticated request attribution,
helper consent, persistence, concurrent requests, bootstrap and crash handling.
Scope file replacement saves without silently widening a document grant. Keep
pending approval and resource failures explicit; no invented success or replay of
uncertain mutations. Apply ordinary Rust/C inheritance and native file/socket
semantics without shell-specific exceptions. Exit: those contracts are implemented
and verified through boundary tests and focused integration evidence.
