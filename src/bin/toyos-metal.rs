//! The metal loop's entry point. Everything it does is
//! [`toyos_build::metal`]; this decides only the exit status.

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
    std::process::exit(2);
}
