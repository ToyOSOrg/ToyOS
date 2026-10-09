//! The HTTPS server a guest's client is judged against: `rustls` on `ring` on
//! the host, listening on loopback and reached from the guest through slirp at
//! [`HOST`], presenting a certificate an [`Authority`] made for this run.
//!
//! It is the client's peer, not its oracle: both ends are `rustls` on `ring`.
//! What it reports is what only the peer sees — that a connection arrived,
//! the protocol version the client agreed to and the `User-Agent` it sent.

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, DnType, IsCa, KeyPair, KeyUsagePurpose, SanType};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// The host as slirp shows it to the guest; it carries a connection to this
/// address to the host's loopback.
pub const HOST: &str = "10.0.2.2";

/// The bytes the server answers every request with: several hundred KiB of a
/// fixed xorshift stream, so a dropped, duplicated or reordered segment
/// changes the hash, and the same every run.
pub fn body() -> Vec<u8> {
    const BYTES: usize = 384 * 1024;
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut body = Vec::with_capacity(BYTES);
    while body.len() < BYTES {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        body.extend_from_slice(&state.to_le_bytes());
    }
    body.truncate(BYTES);
    body
}

/// A certificate authority made for this run alone, whose key never leaves
/// this process.
pub struct Authority(CertifiedIssuer<'static, KeyPair>);

impl Authority {
    pub fn new(name: &str) -> Self {
        let mut params = CertificateParams::default();
        params.distinguished_name.push(DnType::CommonName, name);
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::DigitalSignature];
        let key = KeyPair::generate().expect("an authority's key");
        Self(CertifiedIssuer::self_signed(params, key).expect("an authority's certificate"))
    }

    /// Its certificate, as one PEM block.
    pub fn pem(&self) -> String {
        self.0.pem()
    }

    /// A certificate for the one address `san` and its key, signed by this
    /// authority.
    pub fn leaf(&self, san: IpAddr) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let mut params = CertificateParams::default();
        params.subject_alt_names = vec![SanType::IpAddress(san)];
        let key = KeyPair::generate().expect("a server's key");
        let cert = params.signed_by(&key, &self.0).expect("a server's certificate");
        (cert.der().clone(), PrivatePkcs8KeyDer::from(key.serialize_der()).into())
    }
}

/// What the server saw of a connection: [`Seen::Accepted`] as it arrives,
/// then one of the other two as it ends.
#[derive(Debug)]
pub enum Seen {
    Accepted,
    /// A handshake that completed and a request answered with the whole body.
    Served { version: Option<rustls::ProtocolVersion>, path: String, user_agents: Vec<String> },
    /// A handshake the server could not complete.
    Failed,
}

/// A server on a loopback port of its own, answering each connection in turn
/// with [`body`]-shaped bytes, until this process ends.
pub struct Server {
    pub port: u16,
    seen: Receiver<Seen>,
}

impl Server {
    pub fn start(cert: (CertificateDer<'static>, PrivateKeyDer<'static>), body: Arc<Vec<u8>>) -> Result<Self, String> {
        let (chain, key) = cert;
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .and_then(|b| b.with_no_client_auth().with_single_cert(vec![chain], key))
            .map_err(|e| format!("the HTTPS server's configuration: {e}"))?;
        let config = Arc::new(config);
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("the HTTPS server: {e}"))?;
        let port = listener.local_addr().map_err(|e| format!("the HTTPS server's port: {e}"))?.port();
        let (tx, seen) = mpsc::channel();
        // Ends with the process, or once nobody reads what it saw.
        thread::spawn(move || {
            for tcp in listener.incoming() {
                let tcp = tcp.expect("accept a connection on the HTTPS server's port");
                if tx.send(Seen::Accepted).is_err() || tx.send(serve(&config, tcp, &body)).is_err() {
                    return;
                }
            }
        });
        Ok(Self { port, seen })
    }

    /// The next thing seen, waited for up to `within`.
    pub fn next(&self, within: Duration) -> Result<Seen, String> {
        self.seen.recv_timeout(within).map_err(|e| match e {
            RecvTimeoutError::Timeout => format!("the server on port {} saw nothing more within {within:?}", self.port),
            RecvTimeoutError::Disconnected => format!("the HTTPS server on port {} is gone", self.port),
        })
    }

    /// What it has seen past what was read, without waiting.
    pub fn more(&self) -> Vec<Seen> {
        self.seen.try_iter().collect()
    }
}

/// One connection: the handshake, one request read to its blank line, and
/// the body after a header that gives its length.
fn serve(config: &Arc<rustls::ServerConfig>, mut tcp: TcpStream, body: &[u8]) -> Seen {
    let mut conn = rustls::ServerConnection::new(config.clone()).expect("a server connection");
    while conn.is_handshaking() {
        if conn.complete_io(&mut tcp).is_err() {
            return Seen::Failed;
        }
    }
    let version = conn.protocol_version();
    let mut stream = rustls::Stream::new(&mut conn, &mut tcp);
    let mut request = BufReader::new(&mut stream);
    let mut first = String::new();
    request.read_line(&mut first).expect("the request line");
    let path = first.split_whitespace().nth(1).unwrap_or_default().to_string();
    let mut user_agents = Vec::new();
    loop {
        let mut line = String::new();
        request.read_line(&mut line).expect("a request header");
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("user-agent") {
                user_agents.push(value.trim().to_string());
            }
        }
    }
    drop(request);
    let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    stream.write_all(head.as_bytes()).expect("the response's head");
    stream.write_all(body).expect("the response's body");
    stream.flush().expect("the response");
    conn.send_close_notify();
    while conn.wants_write() {
        conn.write_tls(&mut tcp).expect("the close_notify");
    }
    Seen::Served { version, path, user_agents }
}
