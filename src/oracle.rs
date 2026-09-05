//! Fetching the outside judge's disk image, by content hash.
//!
//! **Plain HTTP, because the artifact is named by its SHA-256 and refused when
//! the bytes do not hash to it — the transport is not what is being trusted.**
//! Only Rust and QEMU may be depended on, so `curl` is not available to a
//! committed test and TLS would have cost a crate.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

/// An artifact a test needs, pinned by the digest that decides whether what
/// arrived is it.
pub struct Pinned {
    pub host: &'static str,
    pub path: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    /// What the file is called once it is here; the digest is its directory.
    pub name: &'static str,
}

/// How long a read may stall before the fetch is called dead rather than slow.
const STALL: Duration = Duration::from_secs(120);
const CHUNK: usize = 1 << 20;

impl Pinned {
    /// Where this artifact lives once fetched.
    pub fn cached_at(&self, target: &Path) -> PathBuf {
        target.join("oracle").join(self.sha256).join(self.name)
    }
}

/// The artifact's path on disk, fetching it first if it is not already there.
///
/// A file already at the cached path is hashed rather than believed: the name
/// is a claim about the contents and nothing else enforces it.
pub fn fetch(pinned: &Pinned, target: &Path) -> Result<PathBuf, String> {
    let at = pinned.cached_at(target);
    if at.exists() {
        match digest_of(&at) {
            Ok(sum) if sum == pinned.sha256 => return Ok(at),
            Ok(sum) => {
                fs::remove_file(&at).map_err(|e| format!("removing {}: {e}", at.display()))?;
                eprintln!("oracle: {} hashed {sum}, not its name — fetching again", at.display());
            }
            Err(err) => return Err(err),
        }
    }

    let dir = at.parent().expect("a cached artifact has a directory");
    fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let partial = dir.join(format!("{}.partial", pinned.name));

    let sum = download(pinned, &partial)?;
    if sum != pinned.sha256 {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "oracle: http://{}{} hashed {sum}, and this test only runs the image pinned at {}",
            pinned.host, pinned.path, pinned.sha256
        ));
    }
    fs::rename(&partial, &at).map_err(|e| format!("renaming {}: {e}", partial.display()))?;
    Ok(at)
}

/// One HTTP/1.1 GET, streamed to `into`, returning what it hashed to.
///
/// A redirect is refused rather than followed: the only thing it can redirect
/// to here is HTTPS, which this has no way to speak, so following would be a
/// hang instead of a sentence.
fn download(pinned: &Pinned, into: &Path) -> Result<String, String> {
    let stream = TcpStream::connect((pinned.host, 80))
        .map_err(|e| format!("oracle: connecting to {}: {e}", pinned.host))?;
    stream.set_read_timeout(Some(STALL)).map_err(|e| format!("oracle: read timeout: {e}"))?;
    stream.set_write_timeout(Some(STALL)).map_err(|e| format!("oracle: write timeout: {e}"))?;
    let mut stream = stream;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: toyos-build\r\nConnection: close\r\n\r\n",
        pinned.path, pinned.host
    );
    stream.write_all(request.as_bytes()).map_err(|e| format!("oracle: sending the request: {e}"))?;

    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let read = stream.read(&mut byte).map_err(|e| format!("oracle: reading the reply: {e}"))?;
        if read == 0 {
            return Err("oracle: the server closed before its headers ended".to_string());
        }
        head.push(byte[0]);
        if head.len() > 16 * 1024 {
            return Err("oracle: the server's headers never ended".to_string());
        }
    }
    let head = String::from_utf8_lossy(&head).to_string();
    let status = head.lines().next().unwrap_or_default().to_string();
    if !status.starts_with("HTTP/1.1 200") {
        return Err(format!("oracle: http://{}{} answered {status:?}", pinned.host, pinned.path));
    }
    let length = header(&head, "content-length")
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| format!("oracle: {status:?} carried no usable Content-Length"))?;
    if length != pinned.bytes {
        return Err(format!(
            "oracle: http://{}{} is {length} bytes where the pin says {}",
            pinned.host, pinned.path, pinned.bytes
        ));
    }

    let file = File::create(into).map_err(|e| format!("creating {}: {e}", into.display()))?;
    let mut file = BufWriter::new(file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut got = 0u64;
    loop {
        let read = stream.read(&mut buf).map_err(|e| format!("oracle: reading the body: {e}"))?;
        if read == 0 {
            break;
        }
        got += read as u64;
        if got > length {
            return Err("oracle: the server sent more than it said it would".to_string());
        }
        hasher.update(&buf[..read]);
        file.write_all(&buf[..read]).map_err(|e| format!("writing {}: {e}", into.display()))?;
    }
    file.flush().map_err(|e| format!("flushing {}: {e}", into.display()))?;
    if got != length {
        return Err(format!("oracle: the body stopped at {got} of {length} bytes"));
    }
    Ok(hex(hasher.finalize().as_slice()))
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .find(|line| line.to_ascii_lowercase().starts_with(&format!("{name}:")))
        .and_then(|line| line.split_once(':'))
        .map(|(_, value)| value.trim())
}

fn digest_of(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buf).map_err(|e| format!("reading {}: {e}", path.display()))?;
        if read == 0 {
            return Ok(hex(hasher.finalize().as_slice()));
        }
        hasher.update(&buf[..read]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The Arch Linux install medium the path below names, which carries
/// `bcachefs-tools` 1.39.4 — upstream's own implementation, at the release
/// this crate's format citation names.
///
/// `archive.archlinux.org` keeps every dated ISO forever and
/// `mirror.pkgbuild.com` mirrors it, so this pin does not rot; the digest is
/// the one `sha256sums.txt` publishes beside the image.
pub const ARCH_ISO: Pinned = Pinned {
    host: "mirror.pkgbuild.com",
    path: "/iso/2026.09.01/archlinux-2026.09.01-x86_64.iso",
    sha256: "be8458032f8105e60ee2a3067f950b6e3c007ee51b38dac50e8b48e765561c91",
    bytes: 1_608_286_208,
    name: "archlinux-2026.09.01-x86_64.iso",
};

#[cfg(test)]
mod tests {
    use super::*;

    /// A cached file whose bytes are not what its directory claims is refused
    /// and removed, rather than handed to a test as the pinned artifact.
    #[test]
    fn a_cached_file_is_hashed_and_not_believed() {
        let target = std::env::temp_dir().join(format!("toyos-oracle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&target);
        let pinned = Pinned {
            host: "127.0.0.1",
            path: "/nothing",
            // The SHA-256 of the empty input, so an empty file passes and any
            // other content does not.
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            bytes: 0,
            name: "artifact.bin",
        };
        let at = pinned.cached_at(&target);
        fs::create_dir_all(at.parent().unwrap()).expect("make the cache directory");
        fs::write(&at, b"").expect("write an empty artifact");
        assert_eq!(fetch(&pinned, &target).as_deref(), Ok(at.as_path()));

        fs::write(&at, b"not the artifact").expect("write a wrong artifact");
        let err = fetch(&pinned, &target).expect_err("a wrong cached file must not be used");
        assert!(err.contains("connecting to"), "{err}");
        assert!(!at.exists(), "a cached file that hashed wrong was left in place");
        let _ = fs::remove_dir_all(&target);
    }

    /// The header split is case-insensitive and takes the value, not the line.
    #[test]
    fn headers_are_read_by_name() {
        let head = "HTTP/1.1 200 OK\r\nServer: nginx\r\nContent-Length: 1608286208\r\n\r\n";
        assert_eq!(header(head, "content-length"), Some("1608286208"));
        assert_eq!(header(head, "etag"), None);
    }
}
