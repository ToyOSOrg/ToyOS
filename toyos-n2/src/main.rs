//! n2's own `main` (`src/main.rs` at the commit `Cargo.toml` pins): a binary
//! target of a git dependency is not one cargo builds.

fn main() {
    let exit_code = match n2::run::run() {
        Ok(code) => code,
        Err(err) => {
            println!("n2: error: {}", err);
            1
        }
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}
