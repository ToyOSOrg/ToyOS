//! The metal loop's entry point. Everything it does is
//! [`toyos_build::metal`]; this decides only the exit status, and a refusal
//! exits 2 so that "the loop could not run" is never read as "the boot failed".

use toyos_build::metal::{run, Args, Verdict};

fn main() {
    let words: Vec<String> = std::env::args().skip(1).collect();
    let args = match Args::parse(&words) {
        Ok(args) => args,
        Err(refusal) => {
            eprintln!("toyos-metal: {refusal}");
            std::process::exit(2);
        }
    };
    match run(&args) {
        Ok(None) => {}
        Ok(Some(Verdict::Pass { boot_ms, test })) => {
            let job = test.map(|t| format!(", {t} exit=0")).unwrap_or_default();
            println!("PASS: the machine booted ToyOS in {boot_ms} ms{job}");
        }
        Ok(Some(Verdict::Fail(why))) => {
            println!("FAIL: {why}");
            std::process::exit(1);
        }
        Err(refusal) => {
            eprintln!("toyos-metal: {refusal}");
            std::process::exit(2);
        }
    }
}
