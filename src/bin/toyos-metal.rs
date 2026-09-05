//! The metal loop's entry point. Everything it does is
//! [`toyos_build::metal`]; this decides only the exit status, and it decides
//! two: a boot that failed exits 1 and a loop that could not run exits 2, so
//! "the machine is broken" is never read as "the driver is".

use toyos_build::metal::{run, Args};

fn main() {
    let words: Vec<String> = std::env::args().skip(1).collect();
    let refusal = match Args::parse(&words).and_then(|args| run(&args)) {
        Ok(None) => return,
        Ok(Some(boot_ms)) => {
            println!("PASS: the machine booted ToyOS in {boot_ms} ms");
            return;
        }
        Err(refusal) => refusal,
    };
    eprintln!("toyos-metal: {refusal}");
    std::process::exit(if refusal.about_the_boot() { 1 } else { 2 });
}
