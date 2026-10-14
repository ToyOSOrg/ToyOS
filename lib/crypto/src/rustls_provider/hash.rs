use crate::{
    hash::{Digest, Sha256, Sha384},
    kdf,
    secret::Secret,
};
use alloc::boxed::Box;
use core::marker::PhantomData;
use rustls::crypto::{hash, hmac, tls13};

pub(super) struct Algorithm<H>(PhantomData<H>);
pub(super) static SHA256: Algorithm<Sha256> = Algorithm(PhantomData);
pub(super) static SHA384: Algorithm<Sha384> = Algorithm(PhantomData);
trait TlsHash: Digest + Send + Sync + 'static {
    const ID: hash::HashAlgorithm;
}
impl TlsHash for Sha256 {
    const ID: hash::HashAlgorithm = hash::HashAlgorithm::SHA256;
}
impl TlsHash for Sha384 {
    const ID: hash::HashAlgorithm = hash::HashAlgorithm::SHA384;
}
struct Context<H>(H);
fn finish<H: Digest>(h: H) -> hash::Output {
    let mut out = Secret([0; 64]);
    h.finish_into(&mut out.0[..H::OUTPUT_LEN])
        .expect("fixed digest size");
    hash::Output::new(&out.0[..H::OUTPUT_LEN])
}
impl<H: TlsHash> hash::Context for Context<H> {
    fn fork_finish(&self) -> hash::Output {
        finish(self.0.fork())
    }
    fn fork(&self) -> Box<dyn hash::Context> {
        Box::new(Self(self.0.fork()))
    }
    fn finish(self: Box<Self>) -> hash::Output {
        finish(self.0)
    }
    fn update(&mut self, data: &[u8]) {
        self.0
            .update(data)
            .expect("TLS transcript within SHA limit");
    }
}
impl<H: TlsHash> hash::Hash for Algorithm<H> {
    fn start(&self) -> Box<dyn hash::Context> {
        Box::new(Context(H::new()))
    }
    fn hash(&self, data: &[u8]) -> hash::Output {
        let mut h = H::new();
        h.update(data).expect("TLS input within SHA limit");
        finish(h)
    }
    fn output_len(&self) -> usize {
        H::OUTPUT_LEN
    }
    fn algorithm(&self) -> hash::HashAlgorithm {
        H::ID
    }
}
struct Mac<H: Digest>(kdf::Hmac<H>);
impl<H: TlsHash> hmac::Hmac for Algorithm<H> {
    fn with_key(&self, key: &[u8]) -> Box<dyn hmac::Key> {
        Box::new(Mac(
            kdf::Hmac::<H>::new(key).expect("TLS key within SHA limit")
        ))
    }
    fn hash_output_len(&self) -> usize {
        H::OUTPUT_LEN
    }
}
impl<H: TlsHash> hmac::Key for Mac<H> {
    fn sign_concat(&self, first: &[u8], middle: &[&[u8]], last: &[u8]) -> hmac::Tag {
        let mut mac = self.0.fork();
        for part in core::iter::once(first)
            .chain(middle.iter().copied())
            .chain(core::iter::once(last))
        {
            mac.update(part).expect("TLS input within SHA limit");
        }
        let mut out = Secret([0; 64]);
        mac.finalize(&mut out.0[..H::OUTPUT_LEN])
            .expect("fixed tag size");
        hmac::Tag::new(&out.0[..H::OUTPUT_LEN])
    }
    fn tag_len(&self) -> usize {
        H::OUTPUT_LEN
    }
}
struct Expander<H: Digest>(kdf::Hkdf<H>);
impl<H: TlsHash> tls13::HkdfExpander for Expander<H> {
    fn expand_slice(&self, info: &[&[u8]], out: &mut [u8]) -> Result<(), tls13::OutputLengthError> {
        self.0
            .expand(info, out)
            .map_err(|_| tls13::OutputLengthError)
    }
    fn expand_block(&self, info: &[&[u8]]) -> tls13::OkmBlock {
        let mut out = Secret([0; 64]);
        self.0
            .expand(info, &mut out.0[..H::OUTPUT_LEN])
            .expect("fixed HKDF block size");
        tls13::OkmBlock::new(&out.0[..H::OUTPUT_LEN])
    }
    fn hash_len(&self) -> usize {
        H::OUTPUT_LEN
    }
}
impl<H: TlsHash> tls13::Hkdf for Algorithm<H> {
    fn extract_from_zero_ikm(&self, salt: Option<&[u8]>) -> Box<dyn tls13::HkdfExpander> {
        self.extract_from_secret(salt, &[0; 64][..H::OUTPUT_LEN])
    }
    fn extract_from_secret(
        &self,
        salt: Option<&[u8]>,
        secret: &[u8],
    ) -> Box<dyn tls13::HkdfExpander> {
        Box::new(Expander(
            kdf::Hkdf::<H>::extract(salt.unwrap_or(&[]), secret)
                .expect("TLS input within SHA limit"),
        ))
    }
    fn expander_for_okm(&self, okm: &tls13::OkmBlock) -> Box<dyn tls13::HkdfExpander> {
        Box::new(Expander(
            kdf::Hkdf::<H>::from_prk(okm.as_ref()).expect("suite PRK size"),
        ))
    }
    fn hmac_sign(&self, key: &tls13::OkmBlock, message: &[u8]) -> hmac::Tag {
        hmac::Hmac::with_key(self, key.as_ref()).sign(&[message])
    }
}
