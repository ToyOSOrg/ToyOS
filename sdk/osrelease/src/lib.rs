//! `/system/etc/os-release`: which build an image is, written by the build
//! system into ROOT and read by `/system/bin/supervisor`, libc's `uname` and
//! the sysinfo fork.
//!
//! The freedesktop os-release format (`os-release(5)`), ToyOS's own fields
//! prefixed `TOYOS_` as it asks. The build is the file's one writer, so
//! [`parse`] takes exactly what [`Release`]'s `Display` writes and refuses
//! anything else by name: every field that is not one of [`Release`]'s is a
//! function of them, and a file whose bytes are not that function's is
//! [`Malformed::NotAsWritten`].
//!
//! Every field is a fact of the source — the commit, whether the files were
//! that commit's, the toolchain's key, the commit's own time — and none is the
//! host's or its clock's, so two builds of one tree write the same bytes.
//!
//! ```text
//! NAME="ToyOS"
//! ID="toyos"
//! VERSION_ID="<12-hex commit>"
//! PRETTY_NAME="ToyOS <12-hex commit>[ (dirty)]"
//! BUILD_ID="<40-hex commit>"
//! ARCHITECTURE="x86-64" | "arm64"
//! HOME_URL="https://github.com/ToyOSOrg/ToyOS"
//! TOYOS_TREE="clean" | "dirty"          untracked files count
//! TOYOS_TOOLCHAIN="<16-hex key>"        the sysroot key the image was compiled against
//! TOYOS_COMMIT_TIME="<Unix seconds>"    the commit's committer time
//! ```

#![no_std]
#![forbid(unsafe_code)]

use core::fmt::{self, Write};

macro_rules! path {
    () => {
        "etc/os-release"
    };
}

/// Where ROOT carries it, without a leading slash — that volume's own
/// spelling. [`GUEST_PATH`] is what a process opens.
pub const PATH: &str = path!();

/// The path a process opens.
pub const GUEST_PATH: &str = concat!("/system/", path!());

/// The operating system's name, `NAME` and `uname`'s `sysname`.
pub const NAME: &str = "ToyOS";

/// `ID`, the operating system's identifier.
pub const ID: &str = "toyos";

/// What `/system/bin/supervisor` says a release under, followed by its commit.
pub const SAID: &str = "supervisor: build ";

/// The architecture an image is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
}

impl Arch {
    /// `ARCHITECTURE`'s spelling, which is systemd's.
    fn os_release(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86-64",
            Arch::Aarch64 => "arm64",
        }
    }

    /// `uname`'s `machine`.
    pub fn machine(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }
}

/// Whether the files an image was built from were its commit's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tree {
    Clean,
    Dirty,
}

impl fmt::Display for Tree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Tree::Clean => "clean",
            Tree::Dirty => "dirty",
        })
    }
}

/// `N` lowercase hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hex<const N: usize>([u8; N]);

impl<const N: usize> Hex<N> {
    /// `text`, if it is exactly `N` lowercase hex digits.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = <[u8; N]>::try_from(text.as_bytes()).ok()?;
        digits.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')).then_some(Self(digits))
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.0).expect("hex digits are ASCII")
    }
}

/// Which build an image is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Release {
    pub commit: Hex<40>,
    pub tree: Tree,
    /// The key of the sysroot the image was compiled against, which names its
    /// compiler and std as well as its libc.
    pub toolchain: Hex<16>,
    pub arch: Arch,
    /// The commit's committer time, in Unix seconds.
    pub committed: u64,
}

impl Release {
    /// The commit's first twelve digits.
    pub fn short(&self) -> &str {
        &self.commit.as_str()[..12]
    }

    /// `uname`'s `release`: [`Release::short`], and `-dirty` after it for a
    /// tree that was not that commit's.
    pub fn uname_release(&self) -> UnameRelease<'_> {
        UnameRelease(self)
    }

    /// `PRETTY_NAME`: [`NAME`], [`Release::short`], and ` (dirty)` after it
    /// for a tree that was not that commit's.
    pub fn pretty_name(&self) -> PrettyName<'_> {
        PrettyName(self)
    }
}

/// [`Release::pretty_name`].
pub struct PrettyName<'a>(&'a Release);

impl fmt::Display for PrettyName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{NAME} {}", self.0.short())?;
        match self.0.tree {
            Tree::Clean => Ok(()),
            Tree::Dirty => f.write_str(" (dirty)"),
        }
    }
}

/// [`Release::uname_release`].
pub struct UnameRelease<'a>(&'a Release);

impl fmt::Display for UnameRelease<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.short())?;
        match self.0.tree {
            Tree::Clean => Ok(()),
            Tree::Dirty => f.write_str("-dirty"),
        }
    }
}

/// The file's bytes.
impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "NAME=\"{NAME}\"")?;
        writeln!(f, "ID=\"{ID}\"")?;
        writeln!(f, "VERSION_ID=\"{}\"", self.short())?;
        writeln!(f, "PRETTY_NAME=\"{}\"", self.pretty_name())?;
        writeln!(f, "{BUILD_ID}=\"{}\"", self.commit.as_str())?;
        writeln!(f, "{ARCHITECTURE}=\"{}\"", self.arch.os_release())?;
        writeln!(f, "HOME_URL=\"https://github.com/ToyOSOrg/ToyOS\"")?;
        writeln!(f, "{TOYOS_TREE}=\"{}\"", self.tree)?;
        writeln!(f, "{TOYOS_TOOLCHAIN}=\"{}\"", self.toolchain.as_str())?;
        writeln!(f, "{TOYOS_COMMIT_TIME}=\"{}\"", self.committed)
    }
}

const BUILD_ID: &str = "BUILD_ID";
const ARCHITECTURE: &str = "ARCHITECTURE";
const TOYOS_TREE: &str = "TOYOS_TREE";
const TOYOS_TOOLCHAIN: &str = "TOYOS_TOOLCHAIN";
const TOYOS_COMMIT_TIME: &str = "TOYOS_COMMIT_TIME";

/// The fields a [`Release`] is read from; every other is a function of them.
const FIELDS: [&str; 5] = [BUILD_ID, TOYOS_TREE, TOYOS_TOOLCHAIN, ARCHITECTURE, TOYOS_COMMIT_TIME];

/// Why a file is not one the build wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    NotUtf8,
    Missing(&'static str),
    Duplicate(&'static str),
    /// The field is there, and its value is not one the build writes for it.
    Value(&'static str),
    /// Every field a [`Release`] is read from is, and the file is not what the
    /// build writes for them: another derived field, another field, another
    /// order or another quoting.
    NotAsWritten,
}

/// The release `bytes` name, if they are what the build writes for one.
pub fn parse(bytes: &[u8]) -> Result<Release, Malformed> {
    let text = core::str::from_utf8(bytes).map_err(|_| Malformed::NotUtf8)?;
    let mut found: [Option<&str>; FIELDS.len()] = [None; FIELDS.len()];
    for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
        if let Some(at) = FIELDS.iter().position(|field| *field == key) {
            if found[at].replace(value).is_some() {
                return Err(Malformed::Duplicate(FIELDS[at]));
            }
        }
    }
    let value = |field: &'static str| -> Result<&str, Malformed> {
        let at = FIELDS.iter().position(|f| *f == field).expect("a field this reads");
        let quoted = found[at].ok_or(Malformed::Missing(field))?;
        quoted.strip_prefix('"').and_then(|v| v.strip_suffix('"')).ok_or(Malformed::Value(field))
    };
    let release = Release {
        commit: Hex::parse(value(BUILD_ID)?).ok_or(Malformed::Value(BUILD_ID))?,
        tree: match value(TOYOS_TREE)? {
            "clean" => Tree::Clean,
            "dirty" => Tree::Dirty,
            _ => return Err(Malformed::Value(TOYOS_TREE)),
        },
        toolchain: Hex::parse(value(TOYOS_TOOLCHAIN)?).ok_or(Malformed::Value(TOYOS_TOOLCHAIN))?,
        arch: match value(ARCHITECTURE)? {
            "x86-64" => Arch::X86_64,
            "arm64" => Arch::Aarch64,
            _ => return Err(Malformed::Value(ARCHITECTURE)),
        },
        committed: value(TOYOS_COMMIT_TIME)?.parse().map_err(|_| Malformed::Value(TOYOS_COMMIT_TIME))?,
    };
    let mut rest = Against(text);
    match write!(rest, "{release}") {
        Ok(()) if rest.0.is_empty() => Ok(release),
        _ => Err(Malformed::NotAsWritten),
    }
}

/// A writer that takes only the text it holds, in order, consuming it.
struct Against<'a>(&'a str);

impl Write for Against<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0 = self.0.strip_prefix(s).ok_or(fmt::Error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::{String, ToString};
    use std::vec::Vec;

    fn release(tree: Tree, arch: Arch) -> Release {
        Release {
            commit: Hex::parse("1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b").unwrap(),
            tree,
            toolchain: Hex::parse("0123456789abcdef").unwrap(),
            arch,
            committed: 1_791_089_159,
        }
    }

    fn every() -> [Release; 4] {
        [
            release(Tree::Clean, Arch::X86_64),
            release(Tree::Dirty, Arch::X86_64),
            release(Tree::Clean, Arch::Aarch64),
            release(Tree::Dirty, Arch::Aarch64),
        ]
    }

    /// The file `text` with `field`'s line replaced by `line`, or dropped for `None`.
    fn with(text: &str, field: &str, line: Option<&str>) -> String {
        let mut out = String::new();
        for l in text.lines() {
            match l.split_once('=') {
                Some((key, _)) if key == field => {
                    if let Some(line) = line {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                _ => {
                    out.push_str(l);
                    out.push('\n');
                }
            }
        }
        out
    }

    #[test]
    fn what_the_build_writes_is_what_a_reader_reads() {
        for r in every() {
            assert_eq!(parse(r.to_string().as_bytes()), Ok(r));
        }
    }

    #[test]
    fn uname_names_a_dirty_tree() {
        assert_eq!(release(Tree::Clean, Arch::X86_64).uname_release().to_string(), "1a2b3c4d5e6f");
        assert_eq!(release(Tree::Dirty, Arch::X86_64).uname_release().to_string(), "1a2b3c4d5e6f-dirty");
    }

    /// The bytes, whole: what `os-release(5)` readers other than ours see.
    #[test]
    fn a_dirty_x86_build_reads_so() {
        assert_eq!(
            release(Tree::Dirty, Arch::X86_64).to_string(),
            "NAME=\"ToyOS\"\n\
             ID=\"toyos\"\n\
             VERSION_ID=\"1a2b3c4d5e6f\"\n\
             PRETTY_NAME=\"ToyOS 1a2b3c4d5e6f (dirty)\"\n\
             BUILD_ID=\"1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b\"\n\
             ARCHITECTURE=\"x86-64\"\n\
             HOME_URL=\"https://github.com/ToyOSOrg/ToyOS\"\n\
             TOYOS_TREE=\"dirty\"\n\
             TOYOS_TOOLCHAIN=\"0123456789abcdef\"\n\
             TOYOS_COMMIT_TIME=\"1791089159\"\n"
        );
    }

    /// **The independent oracle: `os-release(5)`'s grammar**, held against
    /// every file the build can write. Each line is a variable assignment
    /// whose name is upper case, digits and underscores; a value is quoted
    /// and carries none of the shell specials the specification makes
    /// a writer escape; `ID` and `VERSION_ID` are lower case, digits, `.`,
    /// `_` and `-` alone; `ARCHITECTURE` is one of systemd's names; and a
    /// vendor field is prefixed with the vendor's name.
    #[test]
    fn every_file_the_build_writes_is_os_release() {
        const STANDARD: [&str; 6] = ["NAME", "ID", "VERSION_ID", "PRETTY_NAME", "BUILD_ID", "HOME_URL"];
        const SYSTEMD_ARCHITECTURES: [&str; 2] = ["x86-64", "arm64"];
        for r in every() {
            let text = r.to_string();
            assert!(text.ends_with('\n'));
            let mut names = Vec::new();
            for line in text.lines() {
                let (name, quoted) = line.split_once('=').unwrap();
                assert!(name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'), "{line}");
                assert!(STANDARD.contains(&name) || name == "ARCHITECTURE" || name.starts_with("TOYOS_"), "{line}");
                let value = quoted.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap();
                assert!(!value.contains(['"', '\\', '$', '`', '\'']), "{line}");
                if name == "ID" || name == "VERSION_ID" {
                    assert!(
                        value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)),
                        "{line}"
                    );
                }
                if name == "ARCHITECTURE" {
                    assert!(SYSTEMD_ARCHITECTURES.contains(&value), "{line}");
                }
                names.push(name.to_string());
            }
            let unique: std::collections::BTreeSet<&String> = names.iter().collect();
            assert_eq!(unique.len(), names.len(), "a field twice in {text}");
        }
    }

    #[test]
    fn a_file_that_is_not_text_is_refused() {
        let mut bytes = release(Tree::Clean, Arch::X86_64).to_string().into_bytes();
        bytes[8] = 0xff;
        assert_eq!(parse(&bytes), Err(Malformed::NotUtf8));
    }

    #[test]
    fn a_missing_or_doubled_field_is_refused_by_its_name() {
        let text = release(Tree::Clean, Arch::X86_64).to_string();
        for field in FIELDS {
            assert_eq!(parse(with(&text, field, None).as_bytes()), Err(Malformed::Missing(field)), "{field}");
            let line = text.lines().find(|l| l.starts_with(&std::format!("{field}="))).unwrap();
            let doubled = std::format!("{text}{line}\n");
            assert_eq!(parse(doubled.as_bytes()), Err(Malformed::Duplicate(field)), "{field}");
        }
    }

    #[test]
    fn a_value_the_build_does_not_write_is_refused_by_its_field() {
        let text = release(Tree::Clean, Arch::X86_64).to_string();
        let refused = [
            (BUILD_ID, "BUILD_ID=\"1A2B3C4D5E6F7A8B9C0D1E2F3A4B5C6D7E8F9A0B\""),
            (BUILD_ID, "BUILD_ID=\"1a2b3c4d5e6f\""),
            (BUILD_ID, "BUILD_ID=1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b"),
            (TOYOS_TREE, "TOYOS_TREE=\"maybe\""),
            (TOYOS_TREE, "TOYOS_TREE=\"clean"),
            (TOYOS_TOOLCHAIN, "TOYOS_TOOLCHAIN=\"0123456789abcdeg\""),
            (ARCHITECTURE, "ARCHITECTURE=\"x86_64\""),
            (ARCHITECTURE, "ARCHITECTURE=\"riscv64\""),
            (TOYOS_COMMIT_TIME, "TOYOS_COMMIT_TIME=\"-1\""),
            (TOYOS_COMMIT_TIME, "TOYOS_COMMIT_TIME=\"18446744073709551616\""),
            (TOYOS_COMMIT_TIME, "TOYOS_COMMIT_TIME=\"1791089159 \""),
        ];
        for (field, line) in refused {
            assert_eq!(parse(with(&text, field, Some(line)).as_bytes()), Err(Malformed::Value(field)), "{line}");
        }
    }

    /// Each field a [`Release`] is read from parses, and the file is still not
    /// the one the build writes.
    #[test]
    fn a_file_the_build_did_not_write_is_refused() {
        let clean = release(Tree::Clean, Arch::X86_64).to_string();
        let dirty = release(Tree::Dirty, Arch::X86_64).to_string();
        let refused = [
            with(&clean, "PRETTY_NAME", Some("PRETTY_NAME=\"ToyOS 1a2b3c4d5e6f (dirty)\"")),
            with(&dirty, "PRETTY_NAME", Some("PRETTY_NAME=\"ToyOS 1a2b3c4d5e6f\"")),
            with(&clean, "VERSION_ID", Some("VERSION_ID=\"ffffffffffff\"")),
            with(&clean, "NAME", Some("NAME=ToyOS")),
            with(&clean, "ID", None),
            with(&clean, "TOYOS_COMMIT_TIME", Some("TOYOS_COMMIT_TIME=\"+1791089159\"")),
            with(&clean, "TOYOS_COMMIT_TIME", Some("TOYOS_COMMIT_TIME=\"01791089159\"")),
            std::format!("{clean}VARIANT=\"x\"\n"),
            std::format!("# a comment\n{clean}"),
            clean.replace('\n', "\r\n"),
            clean.trim_end().to_string(),
            std::format!("{clean}\n"),
            {
                let mut lines: Vec<&str> = clean.lines().collect();
                lines.swap(0, 1);
                lines.iter().map(|l| std::format!("{l}\n")).collect()
            },
        ];
        for text in refused {
            assert_eq!(parse(text.as_bytes()), Err(Malformed::NotAsWritten), "{text}");
        }
    }
}
