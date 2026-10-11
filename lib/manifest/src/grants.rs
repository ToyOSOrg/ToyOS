//! Which folder of the session user's home an installed package is granted,
//! and the store `/system/bin/supervisor` keeps those grants in.
//!
//! **The supervisor is the only writer of [`STORE`].** A grant reaches it as a
//! request on [`PORT`], which the build gives to [`HOLDER`] alone and the
//! supervisor honours only from a login session, by the caller's badge, or as
//! the person at the screen's answer to a launch's question
//! ([`crate::consent`]).
//!
//! **An answer is keyed on the exact binary**: the session's user, the
//! package's name and the SHA-256 of the program the supervisor starts. A
//! package replaced under `/apps`, an update included, holds none until it is
//! answered again. A stored answer is a folder granted, or Deny, which is kept
//! until it is revoked.
//!
//! **A folder is a directory inside the session user's home** ([`folder`]):
//! never the home itself, nor `Apps` or anything in it, where every app keeps
//! its own folder.
//!
//! **What a launch gets** is [`decide`]'s: the stored answer for that exact
//! binary, a granted folder at most the image's `[apps] folder` ceiling; with
//! none, a question where the launch's session holds the screen, and nothing
//! otherwise.
//!
//! ```text
//! toyos-grants 1
//! grant <user> <package> <sha256> <read-only|read-write> <folder>
//! deny <user> <package> <sha256>
//! ```

use std::fmt;

use crate::package::{self, DIGEST_LEN};
use crate::{session_home, MAX_PROGRAM_NAME};

/// The port the supervisor serves grants on, and the name its holder opens.
pub const PORT: &str = "grants";

/// The one row the build lets receive [`PORT`].
pub const HOLDER: &str = "grants";

/// Where the supervisor keeps every grant, and the directory it makes for it.
pub const STORE: &str = "/state/supervisor/grants";
pub const STORE_DIR: &str = "/state/supervisor";

const HEADER: &str = "toyos-grants 1";

/// How many grants the store holds. Policy on the primitive: one per package
/// a user has, and a list that fits one answer ([`MAX_LIST_BYTES`]).
pub const MAX_ENTRIES: usize = 64;

/// The longest store the supervisor reads: every entry at its longest.
pub const MAX_STORE_BYTES: usize = HEADER.len() + 1 + MAX_ENTRIES * MAX_LINE;

/// The longest root a folder's grant carries, without its leading `/`: what
/// `toyos::fs::MAX_GRANT_ROOT` leaves, which the supervisor asserts.
pub const MAX_ROOT: usize = 54;

/// The deepest folder, in components: std finds a capability by probing at
/// most this many (`MAX_CAPABILITY_DEPTH` in `sdk/std/sys/fs.rs`).
pub const MAX_DEPTH: usize = 4;

const MAX_LINE: usize = "grant ".len() + crate::USER.len() + 1 + MAX_PROGRAM_NAME + 1 + DIGEST_LEN + 1 + 11 + 1 + MAX_ROOT + 2;

/// The longest answer to [`Request::List`]: one line per grant.
pub const MAX_LIST_BYTES: usize = MAX_ENTRIES * (MAX_PROGRAM_NAME + 1 + 10 + 1 + MAX_ROOT + 2);

/// What a package may do in its folder. Ordered: a grant is at most the
/// image's ceiling, and a user may give less than was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

impl Access {
    pub fn word(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::ReadWrite => "read-write",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        [Self::ReadOnly, Self::ReadWrite].into_iter().find(|a| a.word() == word)
    }
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.word())
    }
}

/// A folder a launch is granted: a whole path [`folder`] admits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub path: String,
    pub access: Access,
}

/// One stored answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub user: String,
    pub package: String,
    /// The SHA-256 of the program it was given to, lowercase hex.
    pub binary: String,
    pub answer: Answer,
}

/// What the user answered for a package, kept until it is revoked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Granted(Folder),
    Denied,
}

/// Why `path` may not be a package's folder, as a whole sentence; `Ok` for
/// one it may be.
pub fn folder(path: &str) -> Result<(), String> {
    let home = session_home();
    if path.contains(char::is_control) || !package::is_canonical(path) {
        return Err(format!("{path:?} is not a canonical path"));
    }
    if path == home {
        return Err(format!("{home} is the home itself, and a package is granted a folder inside it"));
    }
    let Some(inside) = path.strip_prefix(&home).and_then(|rest| rest.strip_prefix('/')) else {
        return Err(format!("{path} is not inside {home}"));
    };
    if inside.split('/').next() == Some("Apps") {
        return Err(format!("{path} is {home}/Apps or inside it, where every app keeps its own folder"));
    }
    if path.len() - 1 > MAX_ROOT {
        return Err(format!("{path} is longer than the {MAX_ROOT} bytes a grant's root carries"));
    }
    if path.split('/').skip(1).count() > MAX_DEPTH {
        return Err(format!("{path} is deeper than the {MAX_DEPTH} components a program's std reaches"));
    }
    Ok(())
}

/// Why `granted` may not be given under the image's `ceiling`, its
/// `[apps] folder`, as a whole sentence: a folder [`folder`] refuses, one past
/// the ceiling, or any under none. Every grant the supervisor gives or keeps,
/// by command or by the person's answer, is held to it.
pub fn admit(granted: &Folder, ceiling: Option<Access>) -> Result<(), String> {
    let ceiling = ceiling.ok_or("this image grants no package a folder")?;
    folder(&granted.path)?;
    if granted.access > ceiling {
        return Err(format!("{} is past this image's {ceiling}", granted.access));
    }
    Ok(())
}

/// What a launch is given before it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// It starts, holding this folder or none.
    Start(Option<Folder>),
    /// The person at the screen is asked first.
    Ask,
}

/// What a launch of `binary` is given: the stored answer for that exact
/// binary, a folder at most `ceiling`, the image's `[apps] folder`; with
/// none, the question where the launch's session holds the screen. No
/// ceiling is no folder and no question.
pub fn decide(stored: Option<&Entry>, binary: &str, ceiling: Option<Access>, at_screen: bool) -> Decision {
    let Some(ceiling) = ceiling else { return Decision::Start(None) };
    match stored.filter(|entry| entry.binary == binary).map(|entry| &entry.answer) {
        Some(Answer::Granted(folder)) => {
            Decision::Start(Some(Folder { path: folder.path.clone(), access: folder.access.min(ceiling) }))
        }
        Some(Answer::Denied) => Decision::Start(None),
        None if at_screen => Decision::Ask,
        None => Decision::Start(None),
    }
}

/// The working directory a launch granted `folder` starts in: the caller's
/// own where it lies inside the folder, and the folder's root otherwise.
pub fn cwd(folder: &str, caller: &str) -> String {
    let inside = caller == folder || caller.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'));
    match inside && package::is_canonical(caller) {
        true => caller.to_string(),
        false => folder.to_string(),
    }
}

/// Every grant, as [`STORE`] holds them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Store {
    entries: Vec<Entry>,
}

impl Store {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn find(&self, user: &str, package: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.user == user && e.package == package)
    }

    /// Store `entry` in place of any grant to the same user's same package.
    pub fn put(&mut self, entry: Entry) -> Result<(), String> {
        check(&entry)?;
        match self.entries.iter().position(|e| e.user == entry.user && e.package == entry.package) {
            Some(at) => self.entries[at] = entry,
            None if self.entries.len() >= MAX_ENTRIES => {
                return Err(format!("the store already holds {MAX_ENTRIES} grants"));
            }
            None => self.entries.push(entry),
        }
        Ok(())
    }

    /// Take away the user's grant to `package`; whether there was one.
    pub fn revoke(&mut self, user: &str, package: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| !(e.user == user && e.package == package));
        self.entries.len() != before
    }

    pub fn render(&self) -> String {
        let mut out = format!("{HEADER}\n");
        for e in &self.entries {
            out.push_str(&match &e.answer {
                Answer::Granted(folder) => {
                    format!("grant {} {} {} {} {}\n", e.user, e.package, e.binary, folder.access, folder.path)
                }
                Answer::Denied => format!("deny {} {} {}\n", e.user, e.package, e.binary),
            });
        }
        out
    }

    /// Read back what [`render`](Self::render) wrote, refusing by name
    /// anything it could not have: any declared row can write `/state`.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_STORE_BYTES {
            return Err(format!("{STORE} is {} bytes, past the {MAX_STORE_BYTES} its grants fill", text.len()));
        }
        let mut lines = text.split_terminator('\n');
        if lines.next() != Some(HEADER) {
            return Err(format!("{STORE} does not begin `{HEADER}`"));
        }
        let mut store = Store::default();
        for line in lines {
            let mut words = line.splitn(6, ' ');
            let (user, package, binary, answer) = match [(); 6].map(|()| words.next()) {
                [Some("grant"), Some(user), Some(package), Some(binary), Some(access), Some(path)] => {
                    let access = Access::parse(access).ok_or_else(|| format!("{STORE}: {access:?} is no access"))?;
                    (user, package, binary, Answer::Granted(Folder { path: path.to_string(), access }))
                }
                [Some("deny"), Some(user), Some(package), Some(binary), None, None] => {
                    (user, package, binary, Answer::Denied)
                }
                _ => return Err(format!("{STORE}: {line:?} is not a grant")),
            };
            let entry =
                Entry { user: user.to_string(), package: package.to_string(), binary: binary.to_string(), answer };
            if store.find(user, package).is_some() {
                return Err(format!("{STORE}: {user}'s {package} is granted twice"));
            }
            store.put(entry).map_err(|why| format!("{STORE}: {why}"))?;
        }
        Ok(store)
    }
}

/// What an entry has to be for [`Store::render`] to write a line that reads
/// back as it.
fn check(e: &Entry) -> Result<(), String> {
    if e.user != crate::USER {
        return Err(format!("{:?} is not this image's user", e.user));
    }
    if package::package_of(&format!("{}/{}/x", package::DIR, e.package)) != Some(e.package.as_str()) {
        return Err(format!("{:?} is not a package name", e.package));
    }
    if e.binary.len() != DIGEST_LEN || !e.binary.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(format!("{:?} is not a SHA-256 in lowercase hex", e.binary));
    }
    match &e.answer {
        Answer::Granted(granted) => folder(&granted.path),
        Answer::Denied => Ok(()),
    }
}

/// A request on [`PORT`]: the message type is the verb.
pub const MSG_LIST: u32 = 1;
pub const MSG_ADD: u32 = 2;
pub const MSG_REVOKE: u32 = 3;
/// The supervisor's answers, each carrying its text.
pub const MSG_DONE: u32 = 1;
pub const MSG_REFUSED: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    List,
    /// Grant `package` the folder at `path`, to `access`.
    Add { package: String, folder: Folder },
    Revoke { package: String },
}

impl Request {
    /// The message type and its payload: an add's package, access and path,
    /// each ended by a NUL but the path.
    pub fn encode(&self) -> (u32, Vec<u8>) {
        match self {
            Self::List => (MSG_LIST, Vec::new()),
            Self::Add { package, folder } => {
                (MSG_ADD, format!("{package}\0{}\0{}", folder.access, folder.path).into_bytes())
            }
            Self::Revoke { package } => (MSG_REVOKE, package.as_bytes().to_vec()),
        }
    }

    /// `None` for a frame [`encode`](Self::encode) cannot have written.
    pub fn decode(msg_type: u32, payload: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(payload).ok()?;
        match msg_type {
            MSG_LIST if text.is_empty() => Some(Self::List),
            MSG_ADD => {
                let mut parts = text.splitn(3, '\0');
                let (package, access, path) = (parts.next()?, parts.next()?, parts.next()?);
                let folder = Folder { path: path.to_string(), access: Access::parse(access)? };
                Some(Self::Add { package: package.to_string(), folder })
            }
            MSG_REVOKE if !text.is_empty() && !text.contains('\0') => Some(Self::Revoke { package: text.to_string() }),
            _ => None,
        }
    }
}

/// One line of a [`Request::List`] answer.
pub fn listed(entry: &Entry) -> String {
    match &entry.answer {
        Answer::Granted(folder) => format!("{} {} {}\n", entry.package, folder.access, folder.path),
        Answer::Denied => format!("{} denied\n", entry.package),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN: &str = "99fcd8a700000000000000000000000000000000000000000000000000000000";

    fn entry(package: &str, path: &str, access: Access) -> Entry {
        Entry {
            user: "toy".into(),
            package: package.into(),
            binary: BIN.into(),
            answer: Answer::Granted(Folder { path: path.into(), access }),
        }
    }

    fn denied(package: &str) -> Entry {
        Entry { user: "toy".into(), package: package.into(), binary: BIN.into(), answer: Answer::Denied }
    }

    /// The deepest and longest folder a grant carries, at the longest package
    /// name, and a store filled to its bound, read back as written.
    #[test]
    fn the_store_reads_back_as_written_at_every_bound() {
        let longest = format!("/home/toy/{}", "g".repeat(MAX_ROOT - "home/toy/".len()));
        let deepest = "/home/toy/a/b";
        assert_eq!(folder(&longest), Ok(()));
        assert_eq!(folder(deepest), Ok(()));
        let mut store = Store::default();
        store.put(entry(&"n".repeat(MAX_PROGRAM_NAME), &longest, Access::ReadWrite)).unwrap();
        store.put(entry("gbae", deepest, Access::ReadOnly)).unwrap();
        store.put(entry("spaced", "/home/toy/My Games", Access::ReadWrite)).unwrap();
        store.put(denied("refused")).unwrap();
        for i in store.entries().len()..MAX_ENTRIES {
            store.put(entry(&format!("p{i}"), &longest, Access::ReadWrite)).unwrap();
        }
        let text = store.render();
        assert!(text.len() <= MAX_STORE_BYTES, "{} bytes", text.len());
        assert_eq!(Store::parse(&text), Ok(store.clone()));
        let list: String = store.entries().iter().map(listed).collect();
        assert!(list.len() <= MAX_LIST_BYTES, "{} bytes", list.len());
        assert_eq!(store.put(entry("one-more", deepest, Access::ReadOnly)), Err(format!("the store already holds {MAX_ENTRIES} grants")));
        // A grant to a package already granted replaces it, past the bound too.
        store.put(entry("gbae", "/home/toy/Games", Access::ReadWrite)).unwrap();
        assert_eq!(
            store.find("toy", "gbae").unwrap().answer,
            Answer::Granted(Folder { path: "/home/toy/Games".into(), access: Access::ReadWrite })
        );
        assert_eq!(store.entries().len(), MAX_ENTRIES);
    }

    /// Every folder a package may not be granted, each refused by name.
    #[test]
    fn a_folder_outside_the_home_or_of_the_apps_is_refused_by_name() {
        let too_long = format!("/home/toy/{}", "g".repeat(MAX_ROOT - "home/toy/".len() + 1));
        for (path, said) in [
            ("/home/toy", "is the home itself"),
            ("/home/toy/Apps", "where every app keeps its own folder"),
            ("/home/toy/Apps/gbae", "where every app keeps its own folder"),
            ("/home/toy/Apps/other/Data", "where every app keeps its own folder"),
            ("/home", "is not inside /home/toy"),
            ("/", "is not a canonical path"),
            ("/home/toyx/Games", "is not inside /home/toy"),
            ("/home/other/Games", "is not inside /home/toy"),
            ("/state/supervisor", "is not inside /home/toy"),
            ("/home/toy/Games/../Apps", "is not a canonical path"),
            ("/home/toy/./Games", "is not a canonical path"),
            ("/home/toy//Games", "is not a canonical path"),
            ("/home/toy/Games/", "is not a canonical path"),
            ("home/toy/Games", "is not a canonical path"),
            ("/home/toy/Ga\nmes", "is not a canonical path"),
            (&too_long, "longer than the 54 bytes"),
            ("/home/toy/a/b/c", "deeper than the 4 components"),
        ] {
            match folder(path) {
                Err(why) if why.contains(said) => {}
                other => panic!("{path:?}: {other:?}, not {said:?}"),
            }
        }
        // `Apps` is a component, not a prefix.
        assert_eq!(folder("/home/toy/Appsy"), Ok(()));
    }

    /// Bytes `render` cannot have written are refused, each by name.
    #[test]
    fn a_store_render_cannot_have_written_is_refused() {
        let good = format!("{HEADER}\ngrant toy gbae {BIN} read-write /home/toy/Games\n");
        assert!(Store::parse(&good).is_ok());
        for (text, said) in [
            (String::new(), "does not begin"),
            ("toyos-grants 2\n".to_string(), "does not begin"),
            (format!("{HEADER}\nallow toy gbae {BIN} read-write /home/toy/Games\n"), "is not a grant"),
            (format!("{HEADER}\ngrant toy gbae {BIN} read-write\n"), "is not a grant"),
            (format!("{HEADER}\ngrant toy gbae {BIN} write /home/toy/Games\n"), "is no access"),
            (format!("{HEADER}\ngrant root gbae {BIN} read-write /home/toy/Games\n"), "not this image's user"),
            (format!("{HEADER}\ngrant toy ../x {BIN} read-write /home/toy/Games\n"), "not a package name"),
            (format!("{HEADER}\ngrant toy gbae {} read-write /home/toy/Games\n", BIN.to_uppercase()), "lowercase hex"),
            (format!("{HEADER}\ngrant toy gbae abc read-write /home/toy/Games\n"), "lowercase hex"),
            (format!("{HEADER}\ngrant toy gbae {BIN} read-write /home/toy/Apps/gbae\n"), "keeps its own folder"),
            (format!("{good}grant toy gbae {BIN} read-only /home/toy/Music\n"), "granted twice"),
            (format!("{good}deny toy gbae {BIN}\n"), "granted twice"),
            (format!("{HEADER}\ndeny toy gbae {BIN} /home/toy/Games\n"), "is not a grant"),
            (format!("{HEADER}\ndeny toy gbae\n"), "is not a grant"),
            (format!("{HEADER}\ndeny toy gbae abc\n"), "lowercase hex"),
            (format!("{HEADER}\ndeny root gbae {BIN}\n"), "not this image's user"),
            (format!("{HEADER}\n{}", "x".repeat(MAX_STORE_BYTES)), "past the"),
        ] {
            match Store::parse(&text) {
                Err(why) if why.contains(said) => {}
                other => panic!("{text:?}: {other:?}, not {said:?}"),
            }
        }
    }

    /// A revoke takes exactly the one grant it names.
    #[test]
    fn a_revoke_takes_one_grant_and_says_whether_it_did() {
        let mut store = Store::default();
        store.put(entry("gbae", "/home/toy/Games", Access::ReadWrite)).unwrap();
        store.put(entry("paint", "/home/toy/Pictures", Access::ReadOnly)).unwrap();
        store.put(denied("snake")).unwrap();
        assert!(store.revoke("toy", "gbae"));
        assert!(!store.revoke("toy", "gbae"));
        assert!(store.revoke("toy", "snake"), "a Deny is revoked as a grant is");
        assert_eq!(store.entries(), [entry("paint", "/home/toy/Pictures", Access::ReadOnly)]);
    }

    /// The decision, as a table: the stored answer for the exact binary, a
    /// folder at most the ceiling; with none, the question at the screen
    /// alone; no ceiling, never a folder nor a question.
    #[test]
    fn a_launch_gets_the_answer_for_its_exact_binary_and_is_asked_only_at_the_screen() {
        let (rw, ro, deny) = (
            entry("gbae", "/home/toy/Games", Access::ReadWrite),
            entry("gbae", "/home/toy/Games", Access::ReadOnly),
            denied("gbae"),
        );
        let other = "0".repeat(DIGEST_LEN);
        let games = |access| Decision::Start(Some(Folder { path: "/home/toy/Games".into(), access }));
        let (none, ask) = (Decision::Start(None), Decision::Ask);
        use Access::*;
        type Row<'a> = (Option<&'a Entry>, &'a str, Option<Access>, bool, Decision);
        #[rustfmt::skip]
        let table: [Row; 18] = [
            (Some(&rw), BIN, Some(ReadWrite), false, games(ReadWrite)),
            (Some(&rw), BIN, Some(ReadWrite), true, games(ReadWrite)),
            (Some(&ro), BIN, Some(ReadWrite), true, games(ReadOnly)),
            (Some(&rw), BIN, Some(ReadOnly), true, games(ReadOnly)),
            (Some(&rw), BIN, None, true, none.clone()),
            (Some(&rw), &other, Some(ReadWrite), false, none.clone()),
            (Some(&rw), &other, Some(ReadWrite), true, ask.clone()),
            (None, BIN, Some(ReadWrite), false, none.clone()),
            (None, BIN, Some(ReadWrite), true, ask.clone()),
            (None, BIN, Some(ReadOnly), true, ask.clone()),
            (None, BIN, None, false, none.clone()),
            (None, BIN, None, true, none.clone()),
            (Some(&ro), &other, Some(ReadOnly), false, none.clone()),
            (Some(&deny), BIN, Some(ReadWrite), true, none.clone()),
            (Some(&deny), BIN, Some(ReadWrite), false, none.clone()),
            (Some(&deny), BIN, None, true, none.clone()),
            (Some(&deny), &other, Some(ReadWrite), true, ask),
            (Some(&deny), &other, Some(ReadWrite), false, none),
        ];
        for (i, (stored, binary, ceiling, at_screen, want)) in table.into_iter().enumerate() {
            assert_eq!(decide(stored, binary, ceiling, at_screen), want, "row {i}");
        }
    }

    /// The caller's own working directory where it is inside the folder, and
    /// the folder's root otherwise.
    #[test]
    fn a_launch_starts_in_the_callers_directory_inside_the_folder_or_at_its_root() {
        let games = "/home/toy/Games";
        assert_eq!(cwd(games, "/home/toy/Games/GBA"), "/home/toy/Games/GBA");
        assert_eq!(cwd(games, games), games);
        for outside in ["/", "/home/toy", "/home/toy/GamesX", "/home/toy/Games/../Apps", "/home/toy/Games/./x", "/tmp"] {
            assert_eq!(cwd(games, outside), games, "{outside}");
        }
    }

    #[test]
    fn a_request_reads_back_as_written_and_nothing_else_reads() {
        for request in [
            Request::List,
            Request::Add { package: "gbae".into(), folder: Folder { path: "/home/toy/My Games".into(), access: Access::ReadOnly } },
            Request::Revoke { package: "gbae".into() },
        ] {
            let (msg, payload) = request.encode();
            assert_eq!(Request::decode(msg, &payload), Some(request));
        }
        for (msg, payload) in [
            (MSG_LIST, &b"x"[..]),
            (MSG_ADD, b"gbae\0read-write"),
            (MSG_ADD, b"gbae\0write\0/home/toy/Games"),
            (MSG_REVOKE, b""),
            (MSG_REVOKE, b"a\0b"),
            (MSG_REVOKE, &[0xff]),
            (9, b""),
        ] {
            assert_eq!(Request::decode(msg, payload), None, "{msg} {payload:?}");
        }
    }
}
