//! libc's C headers held to its Rust definitions: every function a header
//! declares is one libc defines, with the definition's signature; every
//! function libc defines is declared by a header, save [`UNDECLARED`]; and no
//! two declarations of one name disagree. What is compared is what a call
//! passes, as the two C ABIs ToyOS has decide it: each argument's and the
//! result's class, and an integer's width and signedness.
//!
//! Both sides are read as text. A header is C with its preprocessor lines
//! dropped, `__cplusplus`'s arms with them; a definition is an item marked
//! `no_mangle`. A declaration or a definition this reader cannot take apart is
//! a red naming it, never a skip.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// What libc defines and no header declares, each with why.
const UNDECLARED: &[(&str, &str)] = &[
    ("_start", "the entry the loader starts a C program at"),
    ("rust_eh_personality", "the personality `core`'s unwind tables name"),
    ("_Unwind_Resume", "what the precompiled `alloc`'s landing pads name"),
    ("__cxa_atexit", "the C++ ABI's, which `cxxabi.h` declares"),
    ("__cxa_thread_atexit_impl", "what libc++abi's `__cxa_thread_atexit` calls, which it declares"),
    ("close_socket", "`close`'s arm for a socket, which `close` never calls: issues/libc-close-of-a-socket-ends-the-process.md"),
];

fn libc() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../userland/libc")
}

/// How a value crosses a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Void,
    /// Bytes, and signedness where the type fixes it: `char` and `wchar_t`
    /// differ between the two architectures.
    Int(u8, Option<bool>),
    F32,
    F64,
    /// x87's 80 bits on x86-64, binary128 on AArch64: no Rust type is either,
    /// so only an assembly definition, whose signature is unchecked, has one.
    LongDouble,
    Ptr,
    VaList,
    /// A struct or union by value.
    Aggregate,
}

impl Class {
    fn agrees(self, other: Class) -> bool {
        match (self, other) {
            (Class::Int(a, sa), Class::Int(b, sb)) => a == b && (sa.is_none() || sb.is_none() || sa == sb),
            (a, b) => a == b,
        }
    }
}

#[derive(Clone, Debug)]
struct Signature {
    ret: Class,
    params: Vec<Class>,
    variadic: bool,
}

impl Signature {
    /// Whether a call written against `self` reaches `def` intact. A C `...`
    /// is met by a Rust `...`, or by trailing integers and pointers read from
    /// the registers the variadic arguments arrive in.
    fn reaches(&self, def: &Signature) -> bool {
        if !self.ret.agrees(def.ret) || def.params.len() < self.params.len() {
            return false;
        }
        let fixed = self.params.iter().zip(&def.params).all(|(c, r)| c.agrees(*r));
        let rest = &def.params[self.params.len()..];
        let tail = if self.variadic {
            def.variadic || rest.iter().all(|c| matches!(c, Class::Int(..) | Class::Ptr))
        } else {
            rest.is_empty() && !def.variadic
        };
        fixed && tail
    }

    fn agrees(&self, other: &Signature) -> bool {
        self.ret.agrees(other.ret)
            && self.variadic == other.variadic
            && self.params.len() == other.params.len()
            && self.params.iter().zip(&other.params).all(|(a, b)| a.agrees(*b))
    }
}

// ---- the headers ----

/// `text` without comments, `\` continuations or preprocessor lines, and
/// without the arms `__cplusplus` selects; every other conditional's arms are
/// both kept.
fn preprocess(text: &str) -> String {
    let mut plain = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("/*").into_iter().chain(rest.find("//")).min() {
        plain.push_str(&rest[..at]);
        rest = if rest[at..].starts_with("/*") {
            &rest[at + rest[at..].find("*/").expect("an unterminated comment") + 2..]
        } else {
            &rest[at + rest[at..].find('\n').unwrap_or(rest.len() - at)..]
        };
    }
    plain.push_str(rest);
    let joined = plain.replace("\\\n", " ");
    // Per open conditional: whether it is `__cplusplus`'s, and whether its
    // current arm is kept.
    let mut frames: Vec<(bool, bool)> = Vec::new();
    let mut out = String::new();
    for line in joined.lines() {
        let directive = line.trim_start();
        if let Some(d) = directive.strip_prefix('#') {
            let words: Vec<&str> = d.split_whitespace().collect();
            match words.first().copied() {
                Some("if" | "ifdef" | "ifndef") => {
                    let cplusplus = words.get(1) == Some(&"__cplusplus");
                    frames.push((cplusplus, !(cplusplus && words[0] == "ifdef")));
                }
                Some("else") => {
                    let top = frames.last_mut().expect("#else outside a conditional");
                    if top.0 {
                        top.1 = !top.1;
                    }
                }
                Some("elif") => {}
                Some("endif") => {
                    frames.pop().expect("#endif outside a conditional");
                }
                _ => {}
            }
            continue;
        }
        if frames.iter().all(|f| f.1) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_alphanumeric() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else if c == '"' {
            let start = i;
            i += 1;
            while chars[i] != '"' {
                i += 1;
            }
            i += 1;
            out.push(chars[start..i].iter().collect());
        } else if chars[i..].starts_with(&['.', '.', '.']) {
            out.push("...".to_string());
            i += 3;
        } else {
            out.push(c.to_string());
            i += 1;
        }
    }
    out
}

/// `tokens` with every `__attribute__((...))` and `_Noreturn` taken out.
fn without_attributes(tokens: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == "__attribute__" {
            let mut depth = 0;
            i += 1;
            loop {
                match tokens[i].as_str() {
                    "(" => depth += 1,
                    ")" => depth -= 1,
                    _ => {}
                }
                i += 1;
                if depth == 0 {
                    break;
                }
            }
        } else if tokens[i] == "_Noreturn" {
            i += 1;
        } else {
            out.push(tokens[i].clone());
            i += 1;
        }
    }
    out
}

/// Top-level declarations: the tokens before each `;` outside every bracket,
/// a struct's `{...}` body kept whole, each marked whether it is an inline
/// function's definition, which ends at its body.
fn declarations(tokens: &[String]) -> Vec<(Vec<String>, bool)> {
    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0i32;
    for t in tokens {
        match t.as_str() {
            "{" | "(" | "[" => depth += 1,
            "}" | ")" | "]" => depth -= 1,
            _ => {}
        }
        current.push(t.clone());
        if depth == 0 && t == ";" {
            current.pop();
            out.push((std::mem::take(&mut current), false));
        } else if depth == 0 && t == "}" {
            let open = current.iter().position(|x| x == "{").expect("a `}` with no `{`");
            if open > 0 && current[open - 1] == ")" {
                out.push((std::mem::take(&mut current), true));
            }
        }
    }
    assert!(current.is_empty(), "a header ends inside a declaration: {current:?}");
    out
}

const QUALIFIERS: &[&str] = &["const", "volatile", "restrict", "__restrict", "__restrict__", "register", "extern", "static", "inline", "__inline", "__inline__"];

/// The class of the C type `words` spell, qualifiers and pointer and array
/// declarators already gone, against `typedefs`.
fn c_class(words: &[&str], typedefs: &BTreeMap<String, Class>) -> Option<Class> {
    if words.is_empty() {
        return None;
    }
    if let [kind, _] = words {
        match *kind {
            "struct" | "union" => return Some(Class::Aggregate),
            "enum" => return Some(Class::Int(4, None)),
            _ => {}
        }
    }
    if let [one] = words {
        if let Some(c) = typedefs.get(*one) {
            return Some(*c);
        }
    }
    let unsigned = words.contains(&"unsigned");
    let signed = words.contains(&"signed");
    let base: Vec<&str> = words.iter().copied().filter(|w| *w != "unsigned" && *w != "signed").collect();
    let sign = |fixed: bool| Some(!fixed);
    Some(match base.as_slice() {
        ["void"] => Class::Void,
        ["char"] if unsigned => Class::Int(1, Some(false)),
        ["char"] if signed => Class::Int(1, Some(true)),
        ["char"] => Class::Int(1, None),
        ["_Bool"] | ["bool"] => Class::Int(1, Some(false)),
        ["short"] | ["short", "int"] => Class::Int(2, sign(unsigned)),
        [] | ["int"] => Class::Int(4, sign(unsigned)),
        ["long"] | ["long", "int"] | ["long", "long"] | ["long", "long", "int"] => Class::Int(8, sign(unsigned)),
        ["float"] => Class::F32,
        ["double"] => Class::F64,
        ["long", "double"] => Class::LongDouble,
        _ => return None,
    })
}

/// The class of one parameter or result: its type, whether or not a name
/// follows it.
fn c_param(tokens: &[String], typedefs: &BTreeMap<String, Class>) -> Option<Class> {
    if tokens.iter().any(|t| t == "*" || t == "[" || t == "(") {
        return Some(Class::Ptr);
    }
    let words: Vec<&str> = tokens.iter().map(String::as_str).filter(|w| !QUALIFIERS.contains(w)).collect();
    c_class(&words, typedefs).or_else(|| c_class(&words[..words.len().checked_sub(1)?], typedefs))
}

/// The classes of the C builtins and the headers every header reaches for:
/// clang's own `stddef.h`, `stdint.h`'s widths and `stdarg.h`.
fn builtin_typedefs() -> BTreeMap<String, Class> {
    let mut t = BTreeMap::new();
    for (name, class) in [
        ("size_t", Class::Int(8, Some(false))),
        ("ssize_t", Class::Int(8, Some(true))),
        ("ptrdiff_t", Class::Int(8, Some(true))),
        ("intptr_t", Class::Int(8, Some(true))),
        ("uintptr_t", Class::Int(8, Some(false))),
        ("wchar_t", Class::Int(4, None)),
        ("__WINT_TYPE__", Class::Int(4, None)),
        ("va_list", Class::VaList),
        ("__builtin_va_list", Class::VaList),
    ] {
        t.insert(name.to_string(), class);
    }
    // clang's own type macros, which `stdint.h` may spell its types with: on
    // both targets a least or fast type is the exact one of its width.
    for bits in [8u8, 16, 32, 64] {
        for (signed, s, u) in [(true, "INT", "int"), (false, "UINT", "uint")] {
            let class = Class::Int(bits / 8, Some(signed));
            t.insert(format!("{u}{bits}_t"), class);
            for kind in ["", "_LEAST", "_FAST"] {
                t.insert(format!("__{s}{kind}{bits}_TYPE__"), class);
            }
        }
    }
    for (name, signed) in [("__INTMAX_TYPE__", true), ("__UINTMAX_TYPE__", false), ("__INTPTR_TYPE__", true), ("__UINTPTR_TYPE__", false)] {
        t.insert(name.to_string(), Class::Int(8, Some(signed)));
    }
    t
}

#[derive(Debug)]
struct Declaration {
    header: String,
    signature: Signature,
}

/// Every function prototype under `include`, by name, and the names the
/// headers define inline.
fn headers(include: &Path) -> (BTreeMap<String, Vec<Declaration>>, BTreeSet<String>, Vec<String>) {
    let mut files = Vec::new();
    walk(include, "h", &mut files);
    let mut typedefs = builtin_typedefs();
    let mut all = Vec::new();
    for file in &files {
        let rel = file.strip_prefix(include).unwrap().display().to_string();
        let text = fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        for (decl, inline) in declarations(&without_attributes(&tokens(&preprocess(&text)))) {
            if decl.first().map(String::as_str) == Some("typedef") {
                typedef(&decl[1..], &mut typedefs);
            } else if !decl.is_empty() {
                all.push((rel.clone(), decl, inline));
            }
        }
    }
    let mut prototypes: BTreeMap<String, Vec<Declaration>> = BTreeMap::new();
    let mut inline_names = BTreeSet::new();
    let mut unread = Vec::new();
    for (header, decl, inline) in all {
        // A function's name is the identifier before the first `(` outside a
        // bracket; a declaration with none declares no function.
        // A struct, union or enum body is no prototype.
        if !inline && decl.iter().any(|t| t == "{") {
            continue;
        }
        let Some(open) = decl.iter().position(|t| t == "(") else { continue };
        let name = &decl[open - 1];
        if name == "(" || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            // `(*name)`: a pointer variable, which is no function.
            continue;
        }
        if inline {
            inline_names.insert(name.clone());
            continue;
        }
        let close = matching(&decl, open);
        let parsed = (|| {
            if close != decl.len() - 1 {
                return None;
            }
            let ret = c_param(&decl[..open - 1], &typedefs)?;
            let mut params = Vec::new();
            let mut variadic = false;
            for param in split_commas(&decl[open + 1..close]) {
                match param.as_slice() {
                    [dots] if dots == "..." => variadic = true,
                    [void] if void == "void" => {}
                    [] => {}
                    p => params.push(c_param(p, &typedefs)?),
                }
            }
            Some(Signature { ret, params, variadic })
        })();
        match parsed {
            Some(signature) => prototypes.entry(name.clone()).or_default().push(Declaration { header, signature }),
            None => unread.push(format!("{header}: `{}` is a declaration this reader cannot take apart", decl.join(" "))),
        }
    }
    (prototypes, inline_names, unread)
}

/// Record the typedef `decl` declares (after the keyword).
fn typedef(decl: &[String], typedefs: &mut BTreeMap<String, Class>) {
    let words: Vec<&String> = decl.iter().collect();
    // `(*name)(...)`: a function pointer.
    if let Some(star) = words.windows(2).position(|w| w[0] == "(" && w[1] == "*") {
        typedefs.insert(words[star + 2].clone(), Class::Ptr);
        return;
    }
    // `name[n]`: an array, which a parameter receives as a pointer.
    if let Some(open) = words.iter().position(|w| *w == "[") {
        typedefs.insert(words[open - 1].clone(), Class::Ptr);
        return;
    }
    let name = words.last().expect("an empty typedef").to_string();
    let ty: Vec<&str> = decl[..decl.len() - 1].iter().map(String::as_str).filter(|w| !QUALIFIERS.contains(w)).collect();
    let class = match ty.first().copied() {
        _ if ty.contains(&"*") => Class::Ptr,
        Some("enum") => Class::Int(4, None),
        Some("struct" | "union") => Class::Aggregate,
        _ => c_class(&ty, typedefs).unwrap_or_else(|| panic!("typedef {name}: {ty:?} is no type this reader knows")),
    };
    typedefs.insert(name, class);
}

fn matching(tokens: &[String], open: usize) -> usize {
    let mut depth = 0;
    for (i, t) in tokens.iter().enumerate().skip(open) {
        match t.as_str() {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    panic!("an unclosed parenthesis in {tokens:?}")
}

fn split_commas(tokens: &[String]) -> Vec<Vec<String>> {
    let mut out = vec![Vec::new()];
    let mut depth = 0;
    for t in tokens {
        match t.as_str() {
            "(" | "[" | "<" => depth += 1,
            ")" | "]" | ">" => depth -= 1,
            "," if depth == 0 => {
                out.push(Vec::new());
                continue;
            }
            _ => {}
        }
        out.last_mut().unwrap().push(t.clone());
    }
    out
}

fn walk(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())).map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            walk(&path, ext, out);
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

// ---- the definitions ----

#[derive(Debug)]
struct Definition {
    file: String,
    /// `None` for an assembly body, whose Rust signature says nothing.
    signature: Option<Signature>,
}

/// Every function libc defines under `no_mangle` in the C library's
/// configuration (`std-runtime` off), by name, and every `no_mangle` item this
/// reader cannot take apart.
fn definitions(src: &Path) -> (BTreeMap<String, Definition>, Vec<String>) {
    let mut files = Vec::new();
    walk(src, "rs", &mut files);
    let mut unread = Vec::new();
    let texts: Vec<(String, String)> = files
        .iter()
        .map(|f| {
            let file = f.strip_prefix(src).unwrap().display().to_string();
            let text = expand_exporting_macros(&fs::read_to_string(f).unwrap()).unwrap_or_else(|why| {
                unread.push(format!("{file}: {why}"));
                String::new()
            });
            (file, text)
        })
        .collect();
    let aliases = rust_aliases(&texts);
    let mut defs = BTreeMap::new();
    for (file, text) in &texts {
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if t != "#[no_mangle]" && t != "#[unsafe(no_mangle)]" {
                continue;
            }
            // The attributes around it, and the item after them.
            let mut first = i;
            while first > 0 && lines[first - 1].trim().starts_with("#[") {
                first -= 1;
            }
            let mut item = i + 1;
            while lines[item].trim().starts_with("#[") {
                item += 1;
            }
            let attributes: Vec<&str> = lines[first..item].iter().map(|l| l.trim()).collect();
            if attributes.contains(&"#[cfg(feature = \"std-runtime\")]") {
                continue;
            }
            let naked = attributes.contains(&"#[unsafe(naked)]");
            let mut head = String::new();
            for l in &lines[item..] {
                head.push_str(l.trim());
                head.push(' ');
                if l.contains('{') || l.trim_end().ends_with(';') {
                    break;
                }
            }
            if !head.contains(" fn ") && !head.starts_with("fn ") {
                // A `static` is data, which no prototype declares.
                if !head.contains("static ") {
                    unread.push(format!("{file}:{}: `{}` is a no_mangle item this reader cannot take apart", item + 1, head.trim()));
                }
                continue;
            }
            match rust_signature(&head, &aliases) {
                Some((name, signature)) => {
                    let signature = if naked { None } else { Some(signature) };
                    defs.insert(name, Definition { file: file.clone(), signature });
                }
                None => unread.push(format!("{file}:{}: `{}` is a definition this reader cannot take apart", item + 1, head.trim())),
            }
        }
    }
    (defs, unread)
}

/// `text` with each `macro_rules!` whose body defines a `no_mangle` item
/// expanded at its invocations, and every macro's definition taken out. An
/// exporting macro is one rule, `($( pattern )*) => {$( body )*}`, whose
/// pattern is literals and `$name:fragment`s that each take one token.
fn expand_exporting_macros(text: &str) -> Result<String, String> {
    let mut text = text.to_string();
    while let Some(at) = text.find("macro_rules!") {
        let open = at + text[at..].find('{').ok_or("a macro_rules! with no body")?;
        let close = closing(&text, open)?;
        let name = text[at + "macro_rules!".len()..open].trim().to_string();
        let block = text[open + 1..close].to_string();
        text.replace_range(at..=close, "");
        if !block.contains("no_mangle") {
            continue;
        }
        let repeated = |inner: &str| inner.trim().strip_prefix("$(").and_then(|p| p.strip_suffix(")*")).map(str::to_string);
        let matcher = block.find('(').ok_or_else(|| format!("macro {name} has no matcher"))?;
        let matcher_end = closing(&block, matcher)?;
        let pattern = repeated(&block[matcher + 1..matcher_end]).ok_or_else(|| format!("macro {name}'s matcher is no `$(...)*`"))?;
        let body = matcher_end + block[matcher_end..].find('{').ok_or_else(|| format!("macro {name} has no transcriber"))?;
        let body = repeated(&block[body + 1..closing(&block, body)?]).ok_or_else(|| format!("macro {name}'s transcriber is no `$(...)*`"))?;
        let pattern = macro_tokens(&pattern);
        let call = format!("{name}!");
        while let Some(c) = text.find(&call) {
            let bracket = c + call.len() + text[c + call.len()..].find(['{', '(']).ok_or_else(|| format!("{call} with no arguments"))?;
            let end = closing(&text, bracket)?;
            let expansion = expand(&pattern, &macro_tokens(&text[bracket + 1..end]), &body).map_err(|why| format!("{call}: {why}"))?;
            let semicolon = text[end + 1..].trim_start().starts_with(';');
            let after = if semicolon { end + 1 + text[end + 1..].find(';').unwrap() + 1 } else { end + 1 };
            text.replace_range(c..after, &expansion);
        }
    }
    Ok(text)
}

/// The index of the bracket closing the one at `open`.
fn closing(text: &str, open: usize) -> Result<usize, String> {
    let mut depth = 0;
    for (i, c) in text.char_indices().skip_while(|(i, _)| *i < open) {
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }
    Err(format!("an unclosed bracket at byte {open}"))
}

/// A macro's pattern or arguments as tokens: a path or `$name:fragment` is
/// one, and so is `=>`.
fn macro_tokens(text: &str) -> Vec<String> {
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | ':');
    let mut out = Vec::new();
    let mut rest = text.trim_start();
    while let Some(c) = rest.chars().next() {
        let len = if word(c) {
            rest.find(|c: char| !word(c)).unwrap_or(rest.len())
        } else if rest.starts_with("=>") {
            2
        } else {
            c.len_utf8()
        };
        out.push(rest[..len].to_string());
        rest = rest[len..].trim_start();
    }
    out
}

/// `body` once per entry of `args` that `pattern` matches, each fragment's
/// `$name` replaced by the token it took.
fn expand(pattern: &[String], args: &[String], body: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut i = 0;
    while i < args.len() {
        let mut bound: Vec<(&str, &str)> = Vec::new();
        for p in pattern {
            let arg = args.get(i).ok_or("the arguments end inside an entry")?;
            match p.strip_prefix('$').and_then(|v| v.split_once(':')) {
                Some((name, _fragment)) => bound.push((name, arg)),
                None if p == arg => {}
                None => return Err(format!("`{arg}` where the pattern has `{p}`")),
            }
            i += 1;
        }
        // `$name` is a prefix of `$name_l`: the longer is replaced first.
        bound.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
        let mut expanded = body.to_string();
        for (name, value) in bound {
            expanded = expanded.replace(&format!("${name}"), value);
        }
        out.push_str(&expanded);
        out.push('\n');
    }
    Ok(out)
}

/// Every `type Name = Type;` in libc, by name; a name two files alias to
/// types of different sign (the architectures' `WChar`) is either sign.
fn rust_aliases(texts: &[(String, String)]) -> BTreeMap<String, Class> {
    let mut spelled: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_, text) in texts {
        for line in text.lines() {
            let t = line.trim();
            let t = t.strip_prefix("pub(crate) ").or_else(|| t.strip_prefix("pub ")).unwrap_or(t);
            if let Some(rest) = t.strip_prefix("type ") {
                if let Some((name, ty)) = rest.split_once('=') {
                    spelled.entry(name.trim().to_string()).or_default().push(ty.trim().trim_end_matches(';').to_string());
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    for (name, types) in &spelled {
        let classes: Vec<Class> = types.iter().filter_map(|t| rust_class(t, &BTreeMap::new())).collect();
        let class = match classes.as_slice() {
            [Class::Int(b, _), ..] if classes.iter().all(|c| matches!(c, Class::Int(x, _) if x == b)) && classes.windows(2).any(|w| w[0] != w[1]) => Class::Int(*b, None),
            [c, ..] => *c,
            [] => continue,
        };
        out.insert(name.clone(), class);
    }
    out
}

fn rust_class(ty: &str, aliases: &BTreeMap<String, Class>) -> Option<Class> {
    let ty = ty.trim();
    if ty.starts_with('*') || ty.starts_with('&') || ty.contains("fn(") || ty.starts_with("Option<") {
        return Some(Class::Ptr);
    }
    let last = ty.rsplit("::").next().unwrap();
    let base = last.split('<').next().unwrap();
    Some(match base {
        "!" | "()" => Class::Void,
        "i8" | "c_schar" => Class::Int(1, Some(true)),
        "u8" | "c_uchar" | "bool" => Class::Int(1, Some(false)),
        "c_char" => Class::Int(1, None),
        "i16" | "c_short" => Class::Int(2, Some(true)),
        "u16" | "c_ushort" => Class::Int(2, Some(false)),
        "i32" | "c_int" => Class::Int(4, Some(true)),
        "u32" | "c_uint" => Class::Int(4, Some(false)),
        "i64" | "isize" | "c_long" | "c_longlong" => Class::Int(8, Some(true)),
        "u64" | "usize" | "c_ulong" | "c_ulonglong" => Class::Int(8, Some(false)),
        "f32" => Class::F32,
        "f64" => Class::F64,
        "VaList" => Class::VaList,
        other => match aliases.get(other) {
            Some(c) => *c,
            None if other.chars().next().is_some_and(char::is_uppercase) || other.contains('_') => Class::Aggregate,
            None => return None,
        },
    })
}

/// `pub unsafe extern "C" fn name(params) -> ret`, as one line.
fn rust_signature(head: &str, aliases: &BTreeMap<String, Class>) -> Option<(String, Signature)> {
    // An arrow's `>` is no bracket: `~~` keeps every index where it was.
    let head = head.replace("->", "~~");
    let after = &head[head.find("fn ")? + 3..];
    let open = after.find('(')?;
    let name = after[..open].trim().to_string();
    let mut depth = 0;
    let mut close = None;
    for (i, c) in after.char_indices().skip(open) {
        match c {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let mut params = Vec::new();
    let mut variadic = false;
    let tokens: Vec<String> = after[open + 1..close].chars().map(|c| c.to_string()).collect();
    for param in split_commas(&tokens) {
        let text = param.concat().replace("~~", "->");
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let ty = text.split_once(':').map_or(text, |(_, ty)| ty).trim();
        if ty == "..." {
            variadic = true;
            continue;
        }
        params.push(rust_class(ty, aliases)?);
    }
    let rest = after[close + 1..].trim();
    let ret = match rest.strip_prefix("~~") {
        Some(r) => rust_class(r.split(['{', ';']).next()?.trim().trim_end_matches("where").trim(), aliases)?,
        None => Class::Void,
    };
    Some((name, Signature { ret, params, variadic }))
}

#[test]
fn every_prototype_is_a_definition_and_every_definition_is_declared() {
    let root = libc();
    let (prototypes, inline, mut wrong) = headers(&root.join("include"));
    let (defs, unread) = definitions(&root.join("src"));
    wrong.extend(unread);

    for (name, decls) in &prototypes {
        for pair in decls.windows(2) {
            if !pair[0].signature.agrees(&pair[1].signature) {
                wrong.push(format!("{name}: {} and {} declare it differently: {:?} and {:?}", pair[0].header, pair[1].header, pair[0].signature, pair[1].signature));
            }
        }
        let decl = &decls[0];
        match defs.get(name) {
            None => wrong.push(format!("{name}: {} declares it, and libc defines no such function", decl.header)),
            Some(Definition { signature: Some(def), file }) if !decl.signature.reaches(def) => wrong.push(format!(
                "{name}: {} declares {:?}, and {file} defines {:?}",
                decl.header, decl.signature, def
            )),
            Some(_) => {}
        }
    }
    for (name, def) in &defs {
        let excused = UNDECLARED.iter().any(|(n, _)| n == name);
        let declared = prototypes.contains_key(name) || inline.contains(name);
        if !declared && !excused {
            wrong.push(format!("{name}: {} defines it, and no header declares it", def.file));
        }
        if declared && excused {
            wrong.push(format!("{name}: declared, and still excused in UNDECLARED"));
        }
    }
    for (name, _) in UNDECLARED {
        if !defs.contains_key(*name) {
            wrong.push(format!("{name}: excused in UNDECLARED, and libc no longer defines it"));
        }
    }
    assert!(prototypes.len() > 100 && defs.len() > 100, "{} prototypes, {} definitions read", prototypes.len(), defs.len());
    assert!(wrong.is_empty(), "libc's headers and definitions part in {} places:\n  {}", wrong.len(), wrong.join("\n  "));
}
