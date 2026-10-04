//! `symbolize [--root DIR] [FILE]`: names the frames of a killed program.
//!
//! The kernel records a user frame as the file it ran, the offset in that file
//! and the file's build-id (`toyos_symbols::frame`). This reads lines — FILE,
//! or stdin — writes each back unchanged, and after each frame line writes the
//! function that offset falls in, out of the named file's own symbol table, or
//! why it names none: a file that is another build, one that cannot be read,
//! one with no symbol table. A log is named on any machine by giving `--root`
//! the directory that holds the image's files.
//!
//! **Nothing a line or a file holds makes it fail**: the lookup is
//! `toyos_symbols::name`, the kernel's own and panic-free on any bytes, and
//! every other outcome is a sentence on the line it is about.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use toyos_symbols::frame::{decode, Decoded};
use toyos_symbols::{demangled, name, Unnamed};

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
            Some(root) => root.join(file.trim_start_matches('/')),
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
            Ok((symbol, within)) => format!("{}+{within:#x}", demangled(symbol)),
            Err(Unnamed::NotElf(e)) => format!("? {file}: {e}"),
            Err(Unnamed::OtherBuild { file: Some(id) }) => format!("? {file} is another build: its id is {id}"),
            Err(Unnamed::OtherBuild { file: None }) => format!("? {file} carries no build-id"),
            Err(Unnamed::NoSymbols) => format!("? {file} has no symbol table"),
            Err(Unnamed::NoSymbol) => format!("? no function in {file} holds {:#x}", frame.offset),
        }
    }
}
