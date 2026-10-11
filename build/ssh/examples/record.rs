//! The instrument that made `tests/fixtures/*.transcript`: serve one
//! connection on `127.0.0.1:<port>` with the fixture host key, the scripted
//! driver and a fixed-byte randomness, and write every chunk each side sent.
//! A real `ssh` dials it; nothing runs this at test time.
//!
//! `cargo run -p toyos-ssh --example record -- <port> <seed byte, hex> <transcript> <comment>`

#[path = "../tests/common/driver.rs"]
#[allow(dead_code)]
mod driver;

use std::io::{Read, Write};
use std::net::TcpListener;

use driver::{Keys, Side, Transcript};
use toyos_ssh::Server;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, port, seed, path, comment] = &args[..] else {
        panic!("usage: record <port> <seed byte, hex> <transcript> <comment>");
    };
    let seed = u8::from_str_radix(seed, 16).expect("a hex seed byte");
    let listener = TcpListener::bind(("127.0.0.1", port.parse::<u16>().expect("a port"))).expect("bind");
    let (mut socket, _) = listener.accept().expect("accept");
    #[allow(deprecated)]
    let rng = ring::test::rand::FixedByteRandom { byte: seed };
    let mut server = Server::new(driver::host_key(), Keys::recorded(), rng);
    let mut transcript = Transcript { seed, chunks: Vec::new() };
    let mut buf = vec![0; 64 * 1024];
    loop {
        let out = server.output();
        if !out.is_empty() {
            socket.write_all(&out).expect("write");
            transcript.chunks.push((Side::Server, out));
        }
        let n = socket.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        transcript.chunks.push((Side::Client, buf[..n].to_vec()));
        let result = server.input(&buf[..n]);
        for event in driver::drive(&mut server) {
            eprintln!("event: {event:?}");
        }
        if let Err(refusal) = result {
            eprintln!("refused: {refusal}");
            let out = server.output();
            socket.write_all(&out).expect("write");
            transcript.chunks.push((Side::Server, out));
            break;
        }
    }
    std::fs::write(path, transcript.write(comment)).expect("write the transcript");
}
