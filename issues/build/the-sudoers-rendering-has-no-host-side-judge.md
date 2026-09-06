---
status: open
kind: tooling
opened: 2026-09-06
---

# `Word::sudoers` is judged only by running sudo on the T14

`Word::sudoers` renders a word so that two readers arrive at the argument the
driver hands `sudo`: the sudoers parser unescapes once, then `fnmatch(3)` (or
`regexec(3)` for an argument beginning with `^`) unescapes again. Nothing on the
host checks that chain. `a_sudoers_word_escapes_what_sudoers_reads` pins the
rendering against a string a human wrote, so it agrees with whatever the author
believed — which is how two backslashes shipped, were installed, and refused the
one command the rule exists to permit.

The judge that caught it was `sudo -n /usr/bin/efibootmgr --create …` run on the
machine, and that is still the only one.

Half of a host-side judge needs nothing new: `libc` is already a dependency of
this package, so `fnmatch` can be called on the pattern with the argument. The
other half is the sudoers parser's unescaping, and a model of it written here
would be the author's reading again — the same thing that was wrong — so the
pair would prove the renderer agrees with this file and not with sudo.

**Exit condition**: either sudo's own parser answers (`cvtsudoers` or `visudo -c`
reading back what a rule matches, if either can be made to say it without
installing the file), or the rendering is checked end to end by a run against
`sudo` and this record is replaced by that gate's name. A model of the parser
written from the manual page does not close it.
