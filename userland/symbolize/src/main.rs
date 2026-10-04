//! `symbolize [--root DIR] [FILE]`: names the frames of a killed program.
//!
//! The kernel records a user frame as the file it ran, the offset in that file
//! and the file's build-id (`toyos_symbols::frame`). This reads lines — FILE,
//! or stdin — writes each back unchanged, and after each frame line writes the
//! function that offset falls in, out of the named file's own symbol table, or
//! why it names none: a file that is another build, one that cannot be read,
//! one with no symbol table. A name no build-id vouches for is printed marked
//! `(unchecked: no build-id)`. A log is named on any machine by giving `--root`
//! the directory that holds the image's files; a record whose name would leave
//! that directory is refused.
//!
//! **Nothing a line or a file holds makes it fail**: the lookup is
//! `toyos_symbols::name`, the kernel's own and panic-free on any bytes, and
//! every other outcome is a sentence on the line it is about.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use toyos_symbols::frame::{decode, Decoded};
use toyos_symbols::{name, Unnamed};

const USAGE: &str = "usage: symbolize [--root DIR] [FILE]";

fn main() -> ExitCode {
    let mut root = None;
    let mut input = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match (arg.to_str(), &input) {
            (Some("--root"), _) => match args.next() {
                Some(dir) => root = Some(PathBuf::from(dir)),
                None => return usage(),
            },
            (Some(flag), _) if flag.starts_with('-') => return usage(),
            (_, None) => input = Some(PathBuf::from(arg)),
            (_, Some(_)) => return usage(),
        }
    }
    let reader: Box<dyn Read> = match &input {
        Some(path) => match fs::File::open(path) {
            Ok(file) => Box::new(file),
            Err(e) => {
                let _ = writeln!(io::stderr(), "symbolize: {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => Box::new(io::stdin()),
    };
    let mut namer = Namer { root, files: HashMap::new() };
    match namer.run(BufReader::new(reader), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(io::stderr(), "symbolize: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    let _ = writeln!(io::stderr(), "{USAGE}");
    ExitCode::from(2)
}

struct Namer {
    root: Option<PathBuf>,
    /// Each file a record names, read once: a backtrace names one file many times.
    files: HashMap<String, Result<Vec<u8>, String>>,
}

impl Namer {
    fn run(&mut self, mut input: impl BufRead, mut out: impl Write) -> io::Result<()> {
        let mut line = Vec::new();
        loop {
            line.clear();
            if input.read_until(b'\n', &mut line)? == 0 {
                return out.flush();
            }
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end_matches(['\n', '\r']);
            match decode(text) {
                None => writeln!(out, "{text}")?,
                Some(Err(refused)) => writeln!(out, "{text}  = ? {}", refused.as_str())?,
                Some(Ok(frame)) => writeln!(out, "{text}  = {}", self.name(&frame))?,
            }
        }
    }

    fn name(&mut self, frame: &Decoded<'_>) -> String {
        let file: String = frame.name().collect();
        let path = match &self.root {
            Some(root) => match under(root, &file) {
                Some(path) => path,
                None => return format!("? {file} leaves the root"),
            },
            None => PathBuf::from(&file),
        };
        let bytes = self.files.entry(file.clone()).or_insert_with(|| {
            fs::read(&path).map_err(|e| format!("? {}: {e}", path.display()))
        });
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(why) => return why.clone(),
        };
        match name(bytes, frame.offset, frame.build_id.as_ref()) {
            Ok(named) => named.to_string(),
            Err(Unnamed::NotElf(e)) => format!("? {file}: {e}"),
            Err(Unnamed::OtherBuild { file: Some(id) }) => format!("? {file} is another build: its id is {id}"),
            Err(Unnamed::OtherBuild { file: None }) => format!("? {file} carries no build-id"),
            Err(Unnamed::NoSymbols) => format!("? {file} has no symbol table"),
            Err(Unnamed::NoSymbol) => format!("? no function in {file} holds {:#x}", frame.offset),
        }
    }
}

/// `file`, a path on the machine that ran it, as a path under `root`; `None`
/// when a component would take it out of `root` — `..`, a Windows prefix, or a
/// part this host reads as more than itself once pushed alone, as Windows reads
/// `C:x`, which `PathBuf::push` lets replace the whole path.
fn under(root: &Path, file: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for component in Path::new(file).components() {
        match component {
            Component::Normal(part) if Path::new(part).components().eq([component]) => path.push(part),
            Component::RootDir | Component::CurDir => {}
            Component::Normal(_) | Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_read_under_the_root() {
        assert_eq!(under(Path::new("img"), "/home/a/child"), Some(PathBuf::from("img/home/a/child")));
    }

    #[test]
    fn a_name_that_leaves_the_root_is_refused() {
        assert_eq!(under(Path::new("img"), "/../../etc/passwd"), None);
        assert_eq!(under(Path::new("img"), "/home/../../x"), None);
    }

    #[test]
    fn a_name_some_host_reads_as_a_drive_stays_under_the_root() {
        let root = Path::new("img");
        for file in ["/home/C:x", "/home/C:/x", "/home/C:"] {
            if let Some(path) = under(root, file) {
                let rest = path.strip_prefix(root).unwrap_or_else(|_| panic!("{file} left the root as {path:?}"));
                assert!(rest.components().all(|c| matches!(c, Component::Normal(_))), "{file} as {path:?}");
            }
        }
    }

    #[test]
    fn a_record_that_leaves_the_root_is_refused_on_its_line() {
        let mut namer = Namer { root: Some(PathBuf::from("img")), files: HashMap::new() };
        let mut out = Vec::new();
        let line = "    0x1000  /../../x+0x10 id=-\n";
        namer.run(line.as_bytes(), &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "    0x1000  /../../x+0x10 id=-  = ? /../../x leaves the root\n");
    }
}
