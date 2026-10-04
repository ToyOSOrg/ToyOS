---
status: open
kind: tooling
opened: 2026-09-29
---

# A killed keystore writer leaves an orphan temp in `target/`

`record_by` in `src/keystore.rs` writes a uniquely named temp beside the record
and renames it over. A writer killed between the write and the rename leaves
that temp behind, and because its name is unique nothing later overwrites it: one
orphan per kill.

Exit condition: whatever next writes that record removes the orphans of its
name.
