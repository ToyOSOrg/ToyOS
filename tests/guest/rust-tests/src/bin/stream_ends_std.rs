//! `stream_ends.rs`'s report, in a guest: one line each, then `ok` where it
//! is the one a host's TCP gives, and an exit of 1 where it is not.
//!
//! argv: the address and port of the harness's peer.

#[path = "../stream_ends.rs"]
mod stream_ends;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, host, port] = &args[..] else { panic!("usage: stream_ends_std <host> <peer port>") };
    let port = port.parse().unwrap_or_else(|e| panic!("the port {port}: {e}"));
    let mut report = Vec::new();
    stream_ends::run(host, port, |line| {
        println!("stream_ends_std: {line}");
        report.push(line);
    });
    if report != stream_ends::EXPECTED {
        println!("stream_ends_std: a host's TCP reports\n{}", stream_ends::EXPECTED.join("\n"));
        std::process::exit(1);
    }
    println!("stream_ends_std: ok");
}
