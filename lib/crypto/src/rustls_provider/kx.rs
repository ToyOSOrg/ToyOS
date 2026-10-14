use crate::{Random, ecc, hybrid, mlkem, x25519};
use alloc::boxed::Box;
use rustls::{
    Error, NamedGroup, ProtocolVersion,
    crypto::{
        ActiveKeyExchange, CompletedKeyExchange, SecureRandom, SharedSecret, SupportedKxGroup,
    },
};
#[derive(Debug)]
pub(super) struct Group {
    pub(super) name: NamedGroup,
    pub(super) random: &'static dyn SecureRandom,
}
struct Entropy<'a>(&'a dyn SecureRandom);
impl Random for Entropy<'_> {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), crate::Error> {
        self.0.fill(out).map_err(|_| crate::Error::Entropy)
    }
}
enum Secret {
    X(x25519::SecretKey),
    P256(Box<ecc::p256::SecretKey>),
    P384(Box<ecc::p384::SecretKey>),
    Hybrid {
        kem: Box<mlkem::DecapsulationKey>,
        x: x25519::SecretKey,
    },
}
struct Active {
    secret: Secret,
    public: alloc::vec::Vec<u8>,
    group: NamedGroup,
}
impl SupportedKxGroup for Group {
    fn name(&self) -> NamedGroup {
        self.name
    }
    fn ffdhe_group(&self) -> Option<rustls::ffdhe_groups::FfdheGroup<'static>> {
        None
    }
    fn usable_for_version(&self, version: ProtocolVersion) -> bool {
        self.name != NamedGroup::X25519MLKEM768 || version == ProtocolVersion::TLSv1_3
    }
    fn start_and_complete(&self, peer: &[u8]) -> Result<CompletedKeyExchange, Error> {
        if self.name != NamedGroup::X25519MLKEM768 {
            let kx = self.start()?;
            return Ok(CompletedKeyExchange {
                group: self.name,
                pub_key: kx.pub_key().to_vec(),
                secret: kx.complete(peer)?,
            });
        }
        if peer.len() != 1216 {
            return Err(invalid());
        }
        let kem = mlkem::EncapsulationKey::from_bytes(&peer[..1184]).map_err(|_| invalid())?;
        let mut rng = Entropy(self.random);
        let x = x25519::SecretKey::generate(&mut rng).map_err(|_| Error::FailedToGetRandomBytes)?;
        let mut public = alloc::vec::Vec::with_capacity(1120);
        let xp = x.public_key();
        let ecdh = x
            .agree(&x25519::PublicKey(peer[1184..].try_into().unwrap()))
            .map_err(|_| invalid())?;
        let (ciphertext, kem_secret) = kem
            .encapsulate(&mut rng)
            .map_err(|_| Error::FailedToGetRandomBytes)?;
        public.extend_from_slice(&ciphertext);
        public.extend_from_slice(&xp.0);
        let secret = hybrid::for_tls(kem_secret, ecdh);
        Ok(CompletedKeyExchange {
            group: self.name,
            pub_key: public,
            secret: SharedSecret::from(secret.as_bytes().as_slice()),
        })
    }
    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, Error> {
        let mut rng = Entropy(self.random);
        let (secret, public) = match self.name {
            NamedGroup::X25519 => {
                let s = x25519::SecretKey::generate(&mut rng)
                    .map_err(|_| Error::FailedToGetRandomBytes)?;
                let p = s.public_key().0.to_vec();
                (Secret::X(s), p)
            }
            NamedGroup::secp256r1 => {
                let s = ecc::p256::SecretKey::generate(&mut rng)
                    .map_err(|_| Error::FailedToGetRandomBytes)?;
                let p = s.public_key().as_sec1().to_vec();
                (Secret::P256(Box::new(s)), p)
            }
            NamedGroup::secp384r1 => {
                let s = ecc::p384::SecretKey::generate(&mut rng)
                    .map_err(|_| Error::FailedToGetRandomBytes)?;
                let p = s.public_key().as_sec1().to_vec();
                (Secret::P384(Box::new(s)), p)
            }
            NamedGroup::X25519MLKEM768 => {
                let kem = mlkem::DecapsulationKey::generate(&mut rng)
                    .map_err(|_| Error::FailedToGetRandomBytes)?;
                let x = x25519::SecretKey::generate(&mut rng)
                    .map_err(|_| Error::FailedToGetRandomBytes)?;
                let mut public = alloc::vec::Vec::with_capacity(1216);
                public.extend_from_slice(kem.public_key().as_bytes());
                public.extend_from_slice(&x.public_key().0);
                (
                    Secret::Hybrid {
                        kem: Box::new(kem),
                        x,
                    },
                    public,
                )
            }
            _ => unreachable!("fixed provider group inventory"),
        };
        Ok(Box::new(Active {
            secret,
            public,
            group: self.name,
        }))
    }
}
impl ActiveKeyExchange for Active {
    fn group(&self) -> NamedGroup {
        self.group
    }
    fn pub_key(&self) -> &[u8] {
        &self.public
    }
    fn ffdhe_group(&self) -> Option<rustls::ffdhe_groups::FfdheGroup<'static>> {
        None
    }
    fn complete(self: Box<Self>, peer: &[u8]) -> Result<SharedSecret, Error> {
        match self.secret {
            Secret::X(s) => {
                let p = x25519::PublicKey(peer.try_into().map_err(|_| invalid())?);
                let v = s.agree(&p).map_err(|_| invalid())?;
                Ok(SharedSecret::from(v.as_bytes().as_slice()))
            }
            Secret::P256(s) => {
                let p = ecc::p256::PublicKey::from_sec1(peer).map_err(|_| invalid())?;
                let v = s.agree(&p).map_err(|_| invalid())?;
                Ok(SharedSecret::from(v.as_bytes().as_slice()))
            }
            Secret::Hybrid { kem, x } => {
                if peer.len() != 1120 {
                    return Err(invalid());
                }
                let ecdh = x
                    .agree(&x25519::PublicKey(peer[1088..].try_into().unwrap()))
                    .map_err(|_| invalid())?;
                let kem = kem.decapsulate(&peer[..1088]).map_err(|_| invalid())?;
                let secret = hybrid::for_tls(kem, ecdh);
                Ok(SharedSecret::from(secret.as_bytes().as_slice()))
            }
            Secret::P384(s) => {
                let p = ecc::p384::PublicKey::from_sec1(peer).map_err(|_| invalid())?;
                let v = s.agree(&p).map_err(|_| invalid())?;
                Ok(SharedSecret::from(v.as_bytes().as_slice()))
            }
        }
    }
}

fn invalid() -> Error {
    rustls::PeerMisbehaved::InvalidKeyShare.into()
}
