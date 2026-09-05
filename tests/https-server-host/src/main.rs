//! The host end of the `https_tls13` judge: one CA and four TLS servers, all
//! minted at start-up so no key or certificate is ever committed.
//!
//! Each server answers one fixed body over one HTTP/1.1 response and differs
//! from the others only in what the client should refuse it for — a name it
//! does not have, a validity window that has passed, a protocol version this
//! client will not speak. The body is the same bytes on every port so the
//! guest and the host arm hash the same thing.
//!
//! stdout is the harness's contract: `ca <path>`, `body-bytes <n>`,
//! `body-sha256 <hex>`, one `port <role> <n>` per server, then `ready`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use der::asn1::{Ia5String, OctetString};
use der::{Encode, EncodePem};
use p256::ecdsa::{DerSignature, SigningKey};
use p256::elliptic_curve::Generate;
use p256::pkcs8::EncodePrivateKey;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use sha2::{Digest, Sha256};
use x509_cert::builder::profile::cabf::tls::{CertificateType, Subscriber};
use x509_cert::builder::profile::cabf::Root;
use x509_cert::builder::{Builder, CertificateBuilder};
use x509_cert::ext::pkix::name::{GeneralName, GeneralNames};
use x509_cert::ext::pkix::SubjectAltName;
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfoOwned;
use x509_cert::time::{Time, Validity};
use x509_cert::Certificate;

/// The guest reaches the host at QEMU user-mode networking's gateway address;
/// the host arm of the same fetch reaches it on loopback. One certificate
/// carries both so the two arms differ by their std and by nothing else.
const GUEST_VIEW_OF_HOST: [u8; 4] = [10, 0, 2, 2];
const HOST_LOOPBACK: [u8; 4] = [127, 0, 0, 1];

/// The name the mismatch control's certificate carries instead. `.invalid` is
/// reserved by RFC 2606, so it can never become somebody's real host.
const WRONG_NAME: &str = "not-this-server.invalid";

fn main() {
    let mut out_dir = None;
    let mut body_bytes = 320_000usize;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out_dir = args.get(i).cloned();
            }
            "--body-bytes" => {
                i += 1;
                body_bytes = args[i].parse().expect("--body-bytes takes a number");
            }
            other => panic!("unknown argument {other}"),
        }
        i += 1;
    }
    let out_dir = out_dir.expect("--out is required");

    let ca = Authority::mint();
    let ca_pem = ca.cert.to_pem(der::pem::LineEnding::LF).expect("CA to PEM");
    let ca_path = format!("{out_dir}/ca.pem");
    std::fs::write(&ca_path, ca_pem).expect("write the CA");

    let body = Arc::new(filler(body_bytes));
    let digest = Sha256::digest(&body[..]);
    let mut sha = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(sha, "{byte:02x}");
    }

    let valid = ca.leaf(&addresses(), Age::Valid);
    let expired = ca.leaf(&addresses(), Age::Expired);
    let wrong = ca.leaf(
        &[GeneralName::DnsName(
            Ia5String::new(WRONG_NAME).expect("an IA5 name"),
        )],
        Age::Valid,
    );

    println!("ca {ca_path}");
    println!("body-bytes {}", body.len());
    println!("body-sha256 {sha}");
    serve("ok", &valid, Version::Tls13, Arc::clone(&body));
    serve("wrongname", &wrong, Version::Tls13, Arc::clone(&body));
    serve("expired", &expired, Version::Tls13, Arc::clone(&body));
    serve("tls12", &valid, Version::Tls12, Arc::clone(&body));
    downgrade();
    println!("ready");
    std::io::stdout().flush().expect("flush the contract");

    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn addresses() -> Vec<GeneralName> {
    vec![
        GeneralName::IpAddress(OctetString::new(GUEST_VIEW_OF_HOST).expect("four bytes")),
        GeneralName::IpAddress(OctetString::new(HOST_LOOPBACK).expect("four bytes")),
    ]
}

/// Deterministic filler: the body has to be reproducible across the two arms
/// and large enough that the record layer fragments it, and nothing else.
fn filler(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

enum Age {
    Valid,
    Expired,
}

#[derive(Clone, Copy)]
enum Version {
    Tls12,
    Tls13,
}

struct Leaf {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl Clone for Leaf {
    fn clone(&self) -> Self {
        Leaf {
            chain: self.chain.clone(),
            key: self.key.clone_key(),
        }
    }
}

struct Authority {
    cert: Certificate,
    key: SigningKey,
    name: Name,
}

impl Authority {
    fn mint() -> Self {
        let key = SigningKey::generate_from_rng(&mut rand_core::UnwrapErr(getrandom::SysRng));
        let name: Name = "CN=ToyOS https judge root,O=ToyOS https judge,C=XX".parse().expect("a root name");
        let spki = SubjectPublicKeyInfoOwned::from_key(key.verifying_key()).expect("the CA's SPKI");
        let profile = Root::new(false, name.clone()).expect("the root profile");
        let cert = CertificateBuilder::new(
            profile,
            SerialNumber::from(1u32),
            window(Age::Valid),
            spki,
        )
        .expect("a CA builder")
        .build::<_, DerSignature>(&key)
        .expect("a signed CA");
        Authority { cert, key, name }
    }

    fn leaf(&self, names: &[GeneralName], age: Age) -> Leaf {
        let key = SigningKey::generate_from_rng(&mut rand_core::UnwrapErr(getrandom::SysRng));
        let subject: Name = "CN=ToyOS https judge leaf,C=XX".parse().expect("a leaf name");
        let spki = SubjectPublicKeyInfoOwned::from_key(key.verifying_key()).expect("the leaf SPKI");
        let general: GeneralNames = names.to_vec();
        let profile = Subscriber {
            certificate_type: CertificateType::domain_validated(subject, general.clone())
                .expect("a domain-validated subscriber"),
            issuer: self.name.clone(),
            client_auth: false,
        };
        let mut builder =
            CertificateBuilder::new(profile, SerialNumber::from(2u32), window(age), spki)
                .expect("a leaf builder");
        // The CABF subscriber profile leaves subjectAltName to its caller, and
        // the SAN is the whole subject of three of this judge's four arms.
        builder
            .add_extension(&SubjectAltName(general))
            .expect("the subject alternative names");
        let cert = builder
            .build::<_, DerSignature>(&self.key)
            .expect("a signed leaf");
        let der = cert.to_der().expect("the leaf in DER");
        let ca_der = self.cert.to_der().expect("the CA in DER");
        Leaf {
            chain: vec![CertificateDer::from(der), CertificateDer::from(ca_der)],
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                key.to_pkcs8_der().expect("the leaf key").as_bytes().to_vec(),
            )),
        }
    }
}

fn window(age: Age) -> Validity {
    let now = std::time::SystemTime::now();
    let day = Duration::from_secs(86_400);
    let (from, to) = match age {
        Age::Valid => (now - day, now + day),
        Age::Expired => (now - day * 30, now - day * 29),
    };
    Validity::new(
        Time::try_from(from).expect("a not-before"),
        Time::try_from(to).expect("a not-after"),
    )
}

/// Bind one server and print the port it got. The listener is bound before the
/// port is printed, so the harness never races the accept loop.
fn serve(role: &str, leaf: &Leaf, version: Version, body: Arc<Vec<u8>>) {
    let provider = Arc::new(rustls_rustcrypto::provider());
    let versions: &[&rustls::SupportedProtocolVersion] = match version {
        Version::Tls12 => &[&rustls::version::TLS12],
        Version::Tls13 => &[&rustls::version::TLS13],
    };
    let leaf = leaf.clone();
    let config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(versions)
        .expect("the server's versions")
        .with_no_client_auth()
        .with_single_cert(leaf.chain, leaf.key)
        .expect("the server's certificate");
    let config = Arc::new(config);

    let listener = TcpListener::bind(("0.0.0.0", 0)).expect("a listening port");
    let port = listener.local_addr().expect("the bound address").port();
    println!("port {role} {port}");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let config = Arc::clone(&config);
            let body = Arc::clone(&body);
            std::thread::spawn(move || answer(stream, config, &body));
        }
    });
}

/// The downgrade control, and the only server here that is not rustls: it
/// answers any ClientHello with a TLS 1.2 ServerHello, so the refusal has to
/// come from the client's own version pin rather than from an honest peer
/// declining. rustls names that one `ServerTlsVersionIsDisabledByOurConfig`.
fn downgrade() {
    let listener = TcpListener::bind(("0.0.0.0", 0)).expect("a listening port");
    println!("port downgrade {}", listener.local_addr().expect("bound").port());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                let mut hello = [0u8; 2048];
                let Ok(n) = stream.read(&mut hello) else { return };
                // ClientHello: 5-byte record header, 4-byte handshake header,
                // 2-byte legacy version, 32-byte random, then the session id
                // this ServerHello has to echo back.
                let id_len_at = 43;
                if n < id_len_at + 1 {
                    return;
                }
                let id_len = hello[id_len_at] as usize;
                if n < id_len_at + 1 + id_len {
                    return;
                }
                let session_id = &hello[id_len_at + 1..id_len_at + 1 + id_len];

                let mut sh = vec![0x03, 0x03];
                sh.extend_from_slice(&[0x5au8; 32]);
                sh.push(id_len as u8);
                sh.extend_from_slice(session_id);
                sh.extend_from_slice(&[0xc0, 0x2b, 0x00, 0x00, 0x00]);
                let mut handshake = vec![0x02];
                handshake.extend_from_slice(&(sh.len() as u32).to_be_bytes()[1..]);
                handshake.extend_from_slice(&sh);
                let mut record = vec![0x16, 0x03, 0x03];
                record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
                record.extend_from_slice(&handshake);
                let _ = stream.write_all(&record);
                let _ = stream.flush();
            });
        }
    });
}

fn answer(stream: TcpStream, config: Arc<ServerConfig>, body: &[u8]) {
    let Ok(conn) = ServerConnection::new(config) else {
        return;
    };
    let mut tls = StreamOwned::new(conn, stream);
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        match tls.read(&mut byte) {
            Ok(0) | Err(_) => return,
            Ok(_) => request.push(byte[0]),
        }
        if request.len() > 8192 {
            return;
        }
    }
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if tls.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = tls.write_all(body);
    let _ = tls.flush();
    let _ = tls.sock.shutdown(std::net::Shutdown::Write);
}
