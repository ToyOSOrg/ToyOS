//! An HTTPS client as any Rust program writes one (`https_client`), reading
//! one URL.
//!
//! argv: the URL, then the roots file. It says one line and ends:
//! `https_get: ok bytes=<n> sha256=<hex>` and 0 for a body read to its end,
//! hashed by `ring` over every byte; `https_get: refused <why>` and 2 for a
//! certificate the client refused; anything else panics.

use std::io::Read;
use std::process::ExitCode;

#[path = "../https_client.rs"]
mod https_client;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [url, roots] = args.as_slice() else {
        panic!("usage: https_get <url> <roots file>, not {args:?}");
    };
    let agent = https_client::agent(roots);

    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(e) => {
            let Some(why) = refused(&e) else { panic!("GET {url}: {e}") };
            let why = match why {
                rustls::CertificateError::NotValidForName | rustls::CertificateError::NotValidForNameContext { .. } => {
                    "not-valid-for-name".to_string()
                }
                rustls::CertificateError::UnknownIssuer => "unknown-issuer".to_string(),
                other => format!("{other:?}"),
            };
            println!("https_get: refused {why}");
            return ExitCode::from(2);
        }
    };
    let mut body = response.into_body().into_reader();
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut chunk = vec![0u8; 16 * 1024];
    let mut bytes = 0usize;
    loop {
        let n = body.read(&mut chunk).unwrap_or_else(|e| panic!("the body of {url}: {e}"));
        if n == 0 {
            break;
        }
        digest.update(&chunk[..n]);
        bytes += n;
    }
    println!("https_get: ok bytes={bytes} sha256={}", https_client::hex(digest.finish()));
    ExitCode::SUCCESS
}

/// The certificate `rustls` refused, where that is what `e` is: ureq hands a
/// handshake's error back inside the I/O error of the read that met it.
fn refused(e: &ureq::Error) -> Option<&rustls::CertificateError> {
    let tls = match e {
        ureq::Error::Rustls(tls) => Some(tls),
        ureq::Error::Io(io) => io.get_ref().and_then(|inner| inner.downcast_ref::<rustls::Error>()),
        _ => None,
    };
    match tls {
        Some(rustls::Error::InvalidCertificate(why)) => Some(why),
        _ => None,
    }
}
