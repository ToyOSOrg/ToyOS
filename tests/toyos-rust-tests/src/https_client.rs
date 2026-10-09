//! An HTTPS client as any Rust program writes one: `ureq` on `rustls` on
//! `ring`, as published, trusting the authorities of one roots file and no
//! others, and naming itself with the project's `User-Agent`.

use ureq::tls::{PemItem, RootCerts, TlsConfig};

/// Root `CLAUDE.md`'s: the only `User-Agent` ToyOS sends.
const USER_AGENT: &str = "toyos-build (https://github.com/ToyOSOrg/ToyOS)";

/// The agent that trusts the certificates of `roots`, a PEM file, and no
/// others.
pub fn agent(roots: &str) -> ureq::Agent {
    let pem = std::fs::read(roots).unwrap_or_else(|e| panic!("read {roots}: {e}"));
    let certs: Vec<_> = ureq::tls::parse_pem(&pem)
        .map(|item| match item.unwrap_or_else(|e| panic!("{roots}: {e}")) {
            PemItem::Certificate(cert) => cert,
            _ => panic!("{roots} holds a PEM block that is not a certificate"),
        })
        .collect();
    let tls = TlsConfig::builder().root_certs(RootCerts::new_with_certs(&certs)).build();
    ureq::Agent::config_builder().user_agent(USER_AGENT).tls_config(tls).build().new_agent()
}

/// A digest as lower-case hex.
pub fn hex(digest: ring::digest::Digest) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}
