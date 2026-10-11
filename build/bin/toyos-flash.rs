//! `toyos-flash`'s entry point; everything it does is [`toyos_build::flash`].

#[macro_use(eprintln)]
extern crate toyos_build;

fn main() {
    let words: Vec<String> = std::env::args().skip(1).collect();
    if let Err(refusal) = toyos_build::flash::run(&words) {
        eprintln!("toyos-flash: {refusal}");
        std::process::exit(1);
    }
}
