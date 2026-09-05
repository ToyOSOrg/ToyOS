//! Fetches one URL with `ureq` over rustls and prints the body's length and SHA-256.
//!
//! The crates are crates.io's, unpatched: this binary exists so that what a
//! Linux program writes is what ToyOS runs. Every outcome is one line —
//! `ok bytes=<n> sha256=<hex>` or `refused <name>`, so a verification failure
//! never reaches the caller as a truncated or empty body.
//!
//! A constraint on scheme, version or authority is set on the client handed to
//! the library, never checked on the argument: the peer chooses every hop after
//! the first. So `https_only` refuses a cleartext hop wherever a redirect puts
//! one, and TLS 1.3 is the only version offered — ureq's connector hardcodes
//! `ALL_VERSIONS`, so it is set on a `ClientConfig` of ours through
//! `Agent::with_parts`, its documented extension point rather than a patch.

use std::io::{Read, Write};
use std::sync::Arc;

use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::{CertificateDer, ServerName};
use sha2::{Digest, Sha256};
use ureq::config::Config;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, Either, LazyBuffers, NextTimeout, TcpConnector,
    Transport, TransportAdapter,
};
use ureq::{Agent, Error};

/// A body larger than this is refused rather than buffered: the caller asked
/// for a hash of what it fetched, and an unbounded read answers a hostile
/// server with the guest's whole heap.
const MAX_BODY: u64 = 16 * 1024 * 1024;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut url = None;
    let mut ca_path = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--ca" => {
                i += 1;
                ca_path = args.get(i).cloned();
            }
            other => url = Some(other.to_string()),
        }
        i += 1;
    }
    let Some(url) = url else {
        println!("https_fetch: usage: https_fetch <url> [--ca <pem>]");
        std::process::exit(2);
    };

    match fetch(&url, ca_path.as_deref()) {
        Ok((len, hash)) => println!("https_fetch: ok bytes={len} sha256={hash}"),
        Err(reason) => {
            println!("https_fetch: refused {reason}");
            if reason.starts_with("unclassified") {
                std::process::exit(1);
            }
        }
    }
}

fn fetch(url: &str, ca_path: Option<&str>) -> Result<(usize, String), String> {
    let roots = roots(ca_path)?;
    let agent = agent(roots)?;

    let response = agent.get(url).call().map_err(|e| refusal(&e))?;
    let mut reader = response.into_body().into_reader().take(MAX_BODY + 1);
    let mut body = Vec::new();
    reader
        .read_to_end(&mut body)
        .map_err(|e| format!("unclassified: read body: {e}"))?;
    if body.len() as u64 > MAX_BODY {
        return Err("body-too-large".to_string());
    }

    let digest = Sha256::digest(&body);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok((body.len(), hex))
}

/// Mozilla's roots as any program gets them, plus the caller's extra CA when it
/// named one. The extra root is added, never substituted for verification.
fn roots(ca_path: Option<&str>) -> Result<RootCertStore, String> {
    let mut store = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let Some(path) = ca_path else {
        return Ok(store);
    };
    let pem = std::fs::read(path).map_err(|e| format!("unclassified: read {path}: {e}"))?;
    let certs = pem_certificates(&pem);
    if certs.is_empty() {
        return Err(format!("unclassified: {path} holds no certificate"));
    }
    let (added, ignored) = store.add_parsable_certificates(certs);
    if added == 0 {
        return Err(format!("unclassified: {path}: {ignored} unparsable certificates"));
    }
    Ok(store)
}

/// PEM decoding by hand, because the ToyOS side of this program is meant to be
/// exactly the dependency set the brief names and nothing else.
fn pem_certificates(pem: &[u8]) -> Vec<CertificateDer<'static>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let text = String::from_utf8_lossy(pem);
    let mut out = Vec::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find(BEGIN) {
        let after = &rest[start + BEGIN.len()..];
        let Some(end) = after.find(END) else { break };
        let base64: String = after[..end].chars().filter(|c| !c.is_whitespace()).collect();
        if let Some(der) = base64_decode(&base64) {
            out.push(CertificateDer::from(der));
        }
        rest = &after[end + END.len()..];
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let body = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in body.bytes() {
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn agent(roots: RootCertStore) -> Result<Agent, String> {
    let provider = Arc::new(rustls_rustcrypto::provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| format!("unclassified: rustls versions: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = ()
        .chain(TcpConnector::default())
        .chain(Tls13Connector { config: Arc::new(config) });
    Ok(Agent::with_parts(
        Config::builder().https_only(true).build(),
        connector,
        DefaultResolver::default(),
    ))
}

#[derive(Debug)]
struct Tls13Connector {
    config: Arc<ClientConfig>,
}

impl<In: Transport> Connector<In> for Tls13Connector {
    type Out = Either<In, Tls13Transport>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, Error> {
        let transport = chained.ok_or(Error::ConnectionFailed)?;
        if !details.needs_tls() || transport.is_tls() {
            return Ok(Some(Either::A(transport)));
        }
        let host = details
            .uri
            .authority()
            .ok_or_else(|| Error::BadUri("no authority".to_string()))?
            .host();
        let name: ServerName<'static> = ServerName::try_from(host)
            .map_err(|_| Error::Tls("not a server name"))?
            .to_owned();
        let conn = ClientConnection::new(self.config.clone(), name)?;
        let buffers = LazyBuffers::new(
            details.config.input_buffer_size(),
            details.config.output_buffer_size(),
        );
        Ok(Some(Either::B(Tls13Transport {
            buffers,
            stream: StreamOwned {
                conn,
                sock: TransportAdapter::new(transport.boxed()),
            },
        })))
    }
}

struct Tls13Transport {
    buffers: LazyBuffers,
    stream: StreamOwned<ClientConnection, TransportAdapter>,
}

impl std::fmt::Debug for Tls13Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tls13Transport")
    }
}

impl Transport for Tls13Transport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        self.stream.get_mut().set_timeout(timeout);
        let output = &self.buffers.output()[..amount];
        self.stream.write_all(output)?;
        Ok(())
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        self.stream.get_mut().set_timeout(timeout);
        let input = self.buffers.input_append_buf();
        let amount = self.stream.read(input)?;
        self.buffers.input_appended(amount);
        Ok(amount > 0)
    }

    fn is_open(&mut self) -> bool {
        self.stream.get_mut().get_mut().is_open()
    }

    fn is_tls(&self) -> bool {
        true
    }
}

/// rustls reaches the caller as an `io::Error` carrying the real one, so the
/// name comes from the downcast and never from the message text.
fn refusal(err: &Error) -> String {
    if let Error::RequireHttpsOnly(_) = err {
        return "plain-http".to_string();
    }
    if let Error::Io(io) = err {
        if let Some(tls) = io.get_ref().and_then(|e| e.downcast_ref::<rustls::Error>()) {
            return tls_refusal(tls);
        }
    }
    format!("unclassified: {err}")
}

fn tls_refusal(err: &rustls::Error) -> String {
    use rustls::CertificateError as C;
    use rustls::Error as E;
    use rustls::PeerIncompatible as P;
    match err {
        E::InvalidCertificate(C::UnknownIssuer) => "unknown-authority".to_string(),
        E::InvalidCertificate(C::Expired | C::ExpiredContext { .. }) => {
            "certificate-expired".to_string()
        }
        E::InvalidCertificate(C::NotValidForName | C::NotValidForNameContext { .. }) => {
            "hostname-mismatch".to_string()
        }
        E::PeerIncompatible(
            P::Tls12NotOfferedOrEnabled
            | P::ServerTlsVersionIsDisabledByOurConfig
            | P::ServerDoesNotSupportTls12Or13,
        ) => "downgrade-refused".to_string(),
        E::AlertReceived(rustls::AlertDescription::ProtocolVersion) => "tls12-refused".to_string(),
        other => format!("unclassified: tls: {other}"),
    }
}
