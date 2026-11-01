//! The machine's SSH server: a shell, a command, and files both ways. A
//! command run over `exec` is a line for `shell -c`, which the shell, launched
//! in its own manifest row and not this daemon's, runs. On ToyOS each
//! connection's programs run in a login session of its own ([`spawn::Login`]).
//!
//! The protocol is `toyos-ssh`'s. This daemon drives it: a thread for each
//! connection ([`connection`]), the host key, who may log in, and the bounds
//! on clients that have not logged in.

mod connection;
mod program;
mod sftp;
mod spawn;

use std::fs;
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use toyos_ssh::authorized_keys::{self, Entry};
use toyos_ssh::{hostkey, Authorizer, HostKey, Random};

/// Where this machine keeps its SSH identity and the keys it trusts: this
/// daemon's own `HOME`, which the supervisor makes `/state/sshserver`. Machine
/// state, so it is in no user's home and no other row's view; where DATA is a
/// tmpfs the identity lasts one boot, which the fingerprint printed at every
/// start makes visible.
fn state() -> String {
    let home = std::env::home_dir().expect("sshserver: the supervisor starts every service with HOME");
    home.to_str().expect("sshserver: HOME is not UTF-8").to_string()
}

fn host_key_path() -> String {
    format!("{}/host_ed25519", state())
}

/// The file that names who may log in. No other row's view holds it, but what
/// this daemon spawns directly does: see `issues/sshserver-authorized-keys-unprotected.md`.
fn authorized_keys_path() -> String {
    format!("{}/authorized_keys", state())
}

/// Connections that have not authenticated, at most: OpenSSH's `MaxStartups`.
/// One more is closed at once. Each is closed at [`LOGIN_DEADLINE`] whatever
/// it does, a client that stops reading included, and gives its place back.
const UNAUTHENTICATED_CAP: usize = 10;

/// How long a client has to authenticate: OpenSSH's `LoginGraceTime`. It is
/// what bounds a client that only offers keys, which costs it no attempt.
const LOGIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// The operating system's randomness.
pub struct OsRandom;

impl Random for OsRandom {
    fn fill(&mut self, destination: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        getrandom::fill(destination).map_err(|_| toyos_crypto::Error::Entropy)
    }
}

/// Who may log in: the `authorized_keys` file, read fresh at every question,
/// so a key added takes effect without a restart and one removed between an
/// offer and its signature is refused. An unreadable file names nobody. The
/// user name chooses nothing: every login runs as this daemon.
pub struct Keys;

impl Authorizer for Keys {
    fn authorizes(&mut self, _user: &str, key: &[u8; 32]) -> bool {
        fs::read_to_string(authorized_keys_path()).is_ok_and(|text| authorized_keys::authorizes(&text, key))
    }
}

/// The machine's identity, minted once and kept.
///
/// A file that exists but does not parse is refused, never replaced. Minting
/// over it would change the identity every client has pinned without anyone
/// asking, which is the one event a host key exists to make noisy.
fn host_key() -> Result<HostKey, String> {
    let path = host_key_path();
    match fs::read_to_string(&path) {
        Ok(text) => HostKey::from_openssh(&text).map_err(|e| {
            format!("{path} is not an unencrypted OpenSSH Ed25519 key ({e}); refusing to replace it — move it aside to mint a new identity")
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut seed = [0; 32];
            let mut check = [0; 4];
            OsRandom.fill(&mut seed).and_then(|()| OsRandom.fill(&mut check)).map_err(|_| "no randomness to mint a host key with".to_string())?;
            fs::write(&path, hostkey::openssh_private(&seed, check, "sshserver")).map_err(|e| format!("cannot write {path}: {e}"))?;
            println!("sshserver: minted a new host identity at {path}");
            Ok(HostKey::from_seed(&seed))
        }
        Err(e) => Err(format!("cannot read {path}: {e}")),
    }
}

/// How many keys the file names, said once at startup so a key that will
/// never work is visible before somebody tries it. `Err` means nobody can
/// authenticate.
fn authorized_key_count() -> Result<usize, String> {
    let path = authorized_keys_path();
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read {path} ({e})"))?;
    let mut usable = 0;
    for entry in authorized_keys::entries(&text) {
        match entry {
            Entry::Key(_) => usable += 1,
            Entry::Restricted => println!("sshserver: an entry in {path} carries options, which are not implemented: it authorizes nothing"),
            Entry::OtherType(kind) => println!("sshserver: a {kind} key in {path} is not taken: only ssh-ed25519 is"),
            Entry::Unreadable(why) => println!("sshserver: a line in {path} is not read: {why}"),
        }
    }
    if usable == 0 {
        return Err(format!("{path} names no usable key; put an ssh-ed25519 public key in it and start again"));
    }
    Ok(usable)
}

/// A place among the connections that have not authenticated, given back
/// when it drops.
pub struct Unauthenticated(Arc<AtomicUsize>);

impl Unauthenticated {
    fn take(count: &Arc<AtomicUsize>) -> Option<Self> {
        let place = Self(count.clone());
        (count.fetch_add(1, Ordering::AcqRel) < UNAUTHENTICATED_CAP).then_some(place)
    }
}

impl Drop for Unauthenticated {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The listener on port 22, or `None` on a machine with no netstack.
///
/// A machine with no netstack has nothing for this daemon to offer, and
/// `NetstackNotFound` is the only error that means that — std maps it to
/// NotConnected. Every other bind failure panics rather than exiting 0 with a
/// line blaming hardware that is fine.
fn listen() -> Option<TcpListener> {
    match TcpListener::bind("0.0.0.0:22") {
        Ok(listener) => Some(listener),
        Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {
            println!("sshserver: no network on this machine, exiting");
            None
        }
        Err(e) => panic!("sshserver: cannot bind 0.0.0.0:22: {e}"),
    }
}

fn main() {
    println!("sshserver: starting...");
    let Some(mut listener) = listen() else { return };

    // Identity and trust are settled after the bind, so that a machine with no
    // netstack reports the network as the reason it is leaving rather than
    // minting a key it will never present. A machine with no NIC has a
    // netstack, which binds the port: it serves once a link brings one.
    let host = match host_key() {
        Ok(key) => Arc::new(key),
        Err(why) => return println!("sshserver: {why}, exiting"),
    };
    println!("sshserver: host identity {}", host.fingerprint());
    // A daemon that can authenticate nobody is an open port, not a service.
    match authorized_key_count() {
        Ok(count) => println!("sshserver: {count} key(s) authorized in total"),
        Err(why) => return println!("sshserver: {why}, exiting"),
    }

    let unauthenticated = Arc::new(AtomicUsize::new(0));
    println!("sshserver: listening on port 22");
    loop {
        match listener.accept() {
            Ok((stream, addr)) => {
                let Some(place) = Unauthenticated::take(&unauthenticated) else {
                    println!("sshserver: connection from {addr} closed: {UNAUTHENTICATED_CAP} others have not logged in yet");
                    continue;
                };
                println!("sshserver: connection from {addr}");
                let _ = stream.set_nodelay(true);
                let host = host.clone();
                let serving = std::thread::Builder::new()
                    .spawn(move || connection::serve(stream, host, Keys, addr.to_string(), place, LOGIN_DEADLINE));
                if let Err(e) = serving {
                    println!("sshserver: connection from {addr} closed: no thread to serve it ({e})");
                }
            }
            // **A listener is a registration netstack holds, and a failed
            // accept is netstack no longer holding it** — netstack was swapped
            // or is gone. Asked again, the same listener answers the same error
            // at once and for ever, so it is dropped and bound anew: through the
            // same port, which the supervisor keeps open across a swap.
            Err(e) => {
                println!("sshserver: the listener on port 22 failed ({e}); binding it again");
                drop(listener);
                let Some(again) = listen() else { return };
                listener = again;
                println!("sshserver: listening on port 22");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// At most [`UNAUTHENTICATED_CAP`] places, and one given back by each that
    /// drops, whichever thread it was.
    #[test]
    fn the_unauthenticated_are_capped() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut places: Vec<_> = (0..UNAUTHENTICATED_CAP).map(|_| Unauthenticated::take(&count).expect("a place under the cap")).collect();
        assert!(Unauthenticated::take(&count).is_none());
        let one = places.pop().unwrap();
        std::thread::spawn(move || drop(one)).join().unwrap();
        let again = Unauthenticated::take(&count);
        assert!(again.is_some());
        assert!(Unauthenticated::take(&count).is_none(), "the one taken again holds its place");
    }
}
