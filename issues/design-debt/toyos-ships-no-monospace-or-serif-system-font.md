---
status: open
kind: finding
opened: 2026-09-26
---

# ToyOS ships no monospace or serif system font

`/system/share/fonts` holds Open Sans alone (four styles). cosmic-text 0.15,
which iced draws text through, names its generic families by fixed names —
`set_sans_serif_family("Open Sans")`, `set_monospace_family("Noto Sans
Mono")`, `set_serif_family("DejaVu Serif")` in `FontSystem::new_with_fonts` —
so an iced app's sans-serif text resolves to the shipped family, and its
monospace and serif text name families the image does not carry and fall to
whatever face the database offers instead, Open Sans, without a word.

Not shown: which face each of those actually resolves to on a guest; no test
draws monospace or serif text.

**Exit**: the image carries a monospace and a serif family that the toolkit's
generic names reach, once the owner has decided where fonts live, with a guest
test that draws monospace text and finds it monospaced on the panel.
