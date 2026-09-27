//! `date`: the machine's clock as Unix seconds, which is what a host reads to
//! place this machine's log lines against its own clock — `date -u +%s`'s
//! answer, whatever it is asked.

pub fn main(_: Vec<String>) {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(since) => println!("{}", since.as_secs()),
        Err(e) => {
            eprintln!("date: this machine's clock reads before 1970: {e}");
            std::process::exit(1);
        }
    }
}
