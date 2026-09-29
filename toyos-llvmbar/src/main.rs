//! `toyos-llvmbar <run directory> <Linux capture>`: every stage-3 span's verdict
//! and the bar. Exits 0 when the bar is set, 1 when it is not, 2 when it
//! cannot judge.

use std::path::Path;
use std::process::exit;

use toyos_llvmbar::{judge, show_wall, Capture, SAMPLES};

fn refuse(why: String) -> ! {
    eprintln!("toyos-llvmbar: {why}\nusage: toyos-llvmbar <run directory> <Linux capture>");
    exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, capture] = &args[..] else {
        refuse(format!("two arguments, not {}", args.len()))
    };
    let dir = Path::new(dir);
    if !dir.is_dir() {
        refuse(format!("{} is not a directory", dir.display()));
    }
    let capture = std::fs::read_to_string(capture)
        .map_err(|e| format!("read {capture}: {e}"))
        .and_then(|t| Capture::parse(&t))
        .unwrap_or_else(|why| refuse(why));
    let read = |name: &str| match std::fs::read_to_string(dir.join(name)) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => panic!("read {}: {e}", dir.join(name).display()),
    };
    let run = judge(&read, &capture);
    for v in &run.verdicts {
        if v.valid() {
            println!(
                "{} VALID {}",
                v.span,
                show_wall(v.wall_cs.expect("a valid span has a wall"))
            );
        } else {
            println!("{} INVALID", v.span);
            for why in &v.refusals {
                println!("  {why}");
            }
        }
    }
    let valid = run.verdicts.iter().filter(|v| v.valid()).count();
    if let Some(m) = run
        .verdicts
        .iter()
        .find(|v| v.valid())
        .and_then(|v| v.machine.as_ref())
    {
        println!(
            "machine: {}\n  cmdline {}\n  BIOS {}",
            m.version, m.cmdline, m.bios
        );
    }
    match run.bar_cs {
        Some(bar) => println!("bar: {}, the best of {valid} valid samples", show_wall(bar)),
        None => {
            println!("bar: not set, {valid} of {SAMPLES} valid samples");
            exit(1);
        }
    }
}
