//! `date -u +%s`: the machine's clock as Unix seconds, which is what a host
//! reads to place this machine's log lines against its own clock.

pub fn main(args: Vec<String>) {
    // Any other form would want `strftime`, and is refused rather than
    // answered in a format nobody asked for.
    if args != ["-u", "+%s"] {
        eprintln!("date: only `date -u +%s` is here, and {args:?} is not it");
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
