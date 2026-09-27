//! `date -u +%s`: the machine's clock as Unix seconds, which is what a host
//! reads to place this machine's log lines against its own clock.
//!
//! **That one form and no other.** A `date` that took a format would have to
//! implement `strftime`, and nothing here needs one; any other argument is
//! refused by name rather than printed in some format the caller did not ask
//! for.

pub fn main(args: Vec<String>) {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if words != ["-u", "+%s"] {
        eprintln!("date: only `date -u +%s` is here, and {words:?} is not it");
        std::process::exit(2);
    }
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(since) => println!("{}", since.as_secs()),
        Err(e) => {
            eprintln!("date: this machine's clock reads before 1970: {e}");
            std::process::exit(1);
        }
    }
}
