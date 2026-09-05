//! What an installed package under `/apps/<name>/` says about itself.
//!
//! `/system/bin/pkg` writes one of these and `/system/bin/init` reads it back
//! to resolve a launch, so the format lives beside [`crate::Manifest`] for the
//! same reason: one renderer, one parser, one round-trip test.
//!
//! **Nothing here is a grant.** `/apps` is writable to every program that can
//! name it, so a manifest is a peer's claim about itself: it says which binary
//! *of its own directory* a launch starts. A device, a right and another
//! package's binary have no spelling in this file at all.
//!
//! ```text
//! name = "gbae"
//! version = "0.2.0"
//! digest = "99fcd8a7…"
//! program = "/apps/gbae/gbae"
//! ```

use crate::MAX_PROGRAM_NAME;

/// Where installed packages live, without a trailing slash.
pub const DIR: &str = "/apps";

/// The file the installer writes into each one.
pub const FILE: &str = "manifest.toml";

/// A SHA-256 as this file spells it: lowercase hex, so the length is the
/// digest's rather than a bound anybody chose.
pub const DIGEST_LEN: usize = 64;

/// One installed package's record of itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// The archive this directory was unpacked from, as the release's own
    /// `SHA256SUMS` spelled it.
    pub digest: String,
    /// The whole guest path a launch starts, always a file directly inside
    /// this package's own directory.
    pub program: String,
}

impl Package {
    /// This package's directory, `/apps/<name>`.
    pub fn dir(name: &str) -> String {
        format!("{DIR}/{name}")
    }

    /// Where [`FILE`] sits for a package of this name.
    pub fn path(name: &str) -> String {
        format!("{DIR}/{name}/{FILE}")
    }

    pub fn render(&self) -> Result<String, String> {
        check_name(&self.name)?;
        value("version", &self.version)?;
        check_digest(&self.digest)?;
        if !program_is_inside(&self.name, &self.program) {
            return Err(format!(
                "`program` is {:?}, which is not a file of {}",
                self.program,
                Self::dir(&self.name)
            ));
        }
        Ok(format!(
            "name = {:?}\nversion = {:?}\ndigest = {:?}\nprogram = {:?}\n",
            self.name, self.version, self.digest, self.program
        ))
    }

    /// Read one back, refusing by name anything [`render`](Self::render) could
    /// not have written. Every refusal is a whole sentence: this is the
    /// boundary, and the caller has to say in one line why a directory
    /// somebody else wrote is not a package.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut out = Package::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (key, rest) = line
                .split_once(" = ")
                .ok_or_else(|| format!("{FILE}: {line:?} is not `key = \"value\"`"))?;
            let quoted = rest
                .strip_prefix('"')
                .and_then(|r| r.strip_suffix('"'))
                .ok_or_else(|| format!("{FILE}: `{key}` is not a quoted value"))?;
            let field = match key {
                "name" => &mut out.name,
                "version" => &mut out.version,
                "digest" => &mut out.digest,
                "program" => &mut out.program,
                other => return Err(format!("{FILE}: `{other}` is not a package field")),
            };
            if !field.is_empty() {
                return Err(format!("{FILE}: `{key}` is given twice"));
            }
            *field = quoted.to_string();
        }
        check_name(&out.name)?;
        value("version", &out.version)?;
        check_digest(&out.digest)?;
        if !program_is_inside(&out.name, &out.program) {
            return Err(format!(
                "{FILE}: `program` is {:?}, which is not a file of {}",
                out.program,
                Self::dir(&out.name)
            ));
        }
        Ok(out)
    }
}

/// How many packages a launcher shows. Policy on the primitive: the popup
/// grows upward from the taskbar, so an unbounded listing is a writable
/// directory deciding how far up a screen something is drawn. The rest stay
/// installed and still launch by path.
pub const MAX_LISTED: usize = 16;

/// The packages a launcher shows, given `/apps`'s listing as
/// `(directory name, its manifest.toml)` pairs.
///
/// **An entry appears only where init would agree to start it**, because a
/// button that does nothing is worse than no button.
pub fn listed(entries: &[(String, String)]) -> Vec<Package> {
    let mut out: Vec<Package> = entries
        .iter()
        .filter_map(|(dir, text)| Package::parse(text).ok().filter(|p| p.name == *dir))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.truncate(MAX_LISTED);
    out
}

/// Whether `path` is already the string every path syscall will act on.
///
/// The kernel normalizes before it opens anything (`kernel/src/vfs.rs`'s
/// `normalize` drops an empty component and `.`, and pops on `..`), so a path
/// meaning something else after that is one nothing can both classify and
/// spawn: `/apps/./x` is outside `/apps` to [`package_of`] and inside it to
/// every syscall.
pub fn is_canonical(path: &str) -> bool {
    path.starts_with('/')
        && !path.split('/').skip(1).any(|part| part.is_empty() || part == "." || part == "..")
}

/// The package a launch path belongs to, or `None` for a path outside `/apps`.
/// The one segment after `/apps`, so `..` and an empty segment answer `None`
/// rather than resolving to some other package's row.
pub fn package_of(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(DIR)?.strip_prefix('/')?;
    let (name, tail) = rest.split_once('/')?;
    if tail.is_empty() || name_is_bad(name) {
        return None;
    }
    Some(name)
}

/// Whether `program` names a file directly inside package `name`'s directory.
pub fn program_is_inside(name: &str, program: &str) -> bool {
    if name_is_bad(name) {
        return false;
    }
    let Some(file) = program.strip_prefix(&format!("{DIR}/{name}/")) else { return false };
    !file.is_empty() && !file.contains('/') && file != "." && file != ".."
}

fn name_is_bad(name: &str) -> bool {
    name.is_empty()
        || name.len() > MAX_PROGRAM_NAME
        || name == "."
        || name == ".."
        || name.contains(|c: char| c == '/' || c == '"' || c.is_whitespace())
}

fn check_name(name: &str) -> Result<(), String> {
    if name_is_bad(name) {
        return Err(format!("{FILE}: {name:?} is not a package name"));
    }
    Ok(())
}

fn check_digest(digest: &str) -> Result<(), String> {
    if digest.len() != DIGEST_LEN
        || !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!("{FILE}: {digest:?} is not a lowercase hex SHA-256"));
    }
    Ok(())
}

/// A field that has to survive being written between quotes and read back.
fn value(key: &'static str, text: &str) -> Result<(), String> {
    if text.is_empty() || text.contains(|c: char| c == '"' || c.is_whitespace()) {
        return Err(format!("{FILE}: `{key}` is {text:?}, which would not survive the file"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Package {
        Package {
            name: "gbae".into(),
            version: "0.2.0".into(),
            digest: "99fcd8a7263b5c25cd90cead1baaa7200ef272100fc2226e008a4e8205ba2916".into(),
            program: "/apps/gbae/gbae".into(),
        }
    }

    #[test]
    fn what_the_installer_writes_is_what_init_reads() {
        let rendered = sample().render().expect("render");
        assert_eq!(Package::parse(&rendered).expect("parse"), sample());
    }

    /// The refusal a hostile directory reaches for: a launch pointed outside
    /// its own package.
    #[test]
    fn a_manifest_cannot_name_a_binary_outside_its_own_directory() {
        for program in [
            "/system/bin/init",
            "/apps/other/other",
            "/apps/gbae/../../system/bin/init",
            "/apps/gbae/sub/x",
            "/apps/gbae/",
        ] {
            let bad = Package { program: program.into(), ..sample() };
            assert!(bad.render().is_err(), "{program} was accepted");
        }
        assert!(sample().render().is_ok());
    }

    #[test]
    fn a_field_that_is_not_one_is_refused_by_name() {
        assert!(Package::parse("name = \"gbae\"\n").is_err());
        assert!(Package::parse("name = gbae\n").is_err());
        assert!(Package::parse("owner = \"root\"\n").is_err());
        let twice = format!("{}name = \"other\"\n", sample().render().unwrap());
        assert!(Package::parse(&twice).is_err());
        let short = Package { digest: "99fc".into(), ..sample() };
        assert!(Package::parse(&format!(
            "name = \"gbae\"\nversion = \"0.2.0\"\ndigest = \"{}\"\nprogram = \"/apps/gbae/gbae\"\n",
            short.digest
        ))
        .is_err());
        let upper = sample().digest.to_uppercase();
        assert!(check_digest(&upper).is_err());
    }

    /// The four `package_of` answers `None` for while every path syscall lands
    /// them inside `/apps` — the hole the gate above closes.
    #[test]
    fn a_path_the_normalizer_would_change_is_not_canonical_and_classifies_as_nothing() {
        assert!(is_canonical("/apps/toy/echo"));
        assert!(is_canonical("/system/bin/toybox"));
        for bad in
            ["/apps/./toy/echo", "/apps//toy/echo", "apps/toy/echo", "/tmp/../apps/toy/echo",
             "/apps/toy/echo/", "./echo", "", "/"]
        {
            assert!(!is_canonical(bad), "{bad:?} passed as canonical");
        }
        for e in ["/apps/./toy/echo", "/apps//toy/echo", "apps/toy/echo", "/tmp/../apps/toy/echo"]
        {
            assert_eq!(package_of(e), None, "{e:?} classified as a package");
        }
    }

    /// The launcher shows what init would start, in a bounded list.
    #[test]
    fn a_listing_shows_only_what_init_would_start_and_never_more_than_the_bound() {
        let good = sample().render().unwrap();
        let entries = vec![
            ("zed".to_string(), good.replace("\"gbae\"", "\"zed\"").replace("gbae/", "zed/")),
            ("gbae".to_string(), good.clone()),
            ("notes".to_string(), String::from("this is not a manifest")),
            ("impostor".to_string(), good.clone()),
        ];
        let shown = listed(&entries);
        let names: Vec<&str> = shown.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["gbae", "zed"]);

        let many: Vec<(String, String)> = (0..MAX_LISTED + 5)
            .map(|i| {
                let name = format!("p{i:02}");
                let text = good.replace("\"gbae\"", &format!("{name:?}")).replace("gbae/", &format!("{name}/"));
                (name, text)
            })
            .collect();
        assert_eq!(listed(&many).len(), MAX_LISTED);
    }

    #[test]
    fn a_launch_path_names_one_package_or_none() {
        assert_eq!(package_of("/apps/gbae/gbae"), Some("gbae"));
        assert_eq!(package_of("/apps/gbae/bin/gbae"), Some("gbae"));
        assert_eq!(package_of("/system/bin/init"), None);
        assert_eq!(package_of("/apps/gbae"), None);
        assert_eq!(package_of("/apps//gbae"), None);
        assert_eq!(package_of("/apps/../system/bin/init"), None);
        assert_eq!(package_of(&format!("/apps/{}/x", "n".repeat(MAX_PROGRAM_NAME + 1))), None);
    }
}
