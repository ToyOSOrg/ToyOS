//! HMAC (RFC 2104) and HKDF (RFC 5869), for TLS and SSH key schedules.
//! Contents do not control branches or addresses. Key/message/output lengths
//! are public. Keys, chaining values and temporary digest material are wiped.

use crate::{Error, hash::Digest, secret::Secret};

pub struct Hmac<H: Digest> {
    inner: H,
    outer: H,
}

impl<H: Digest> Hmac<H> {
    #[cfg(feature = "rustls-provider")]
    pub(crate) fn fork(&self) -> Self {
        Self {
            inner: self.inner.fork(),
            outer: self.outer.fork(),
        }
    }
    /// Hashers as `H::new` makes them, to be keyed in place: a returned
    /// `Self` is a copy of both hashers' state.
    #[inline(always)]
    fn unkeyed() -> Self {
        Self { inner: H::new(), outer: H::new() }
    }
    #[inline(always)]
    fn key(&mut self, key: &[u8]) -> Result<(), Error> {
        let mut block = Secret([0u8; 128]);
        if key.len() > H::BLOCK_LEN {
            let mut hash = H::new();
            hash.update(key)?;
            hash.finish_into(&mut block.0[..H::OUTPUT_LEN])?;
        } else {
            block.0[..key.len()].copy_from_slice(key);
        }
        for byte in &mut block.0[..H::BLOCK_LEN] {
            *byte ^= 0x36;
        }
        self.inner.update(&block.0[..H::BLOCK_LEN])?;
        for byte in &mut block.0[..H::BLOCK_LEN] {
            *byte ^= 0x36 ^ 0x5c;
        }
        self.outer.update(&block.0[..H::BLOCK_LEN])
    }
    #[inline(always)]
    fn finish(&mut self, output: &mut [u8]) -> Result<(), Error> {
        if output.len() != H::OUTPUT_LEN {
            return Err(Error::Length);
        }
        let mut digest = Secret([0u8; 64]);
        self.inner.finish_into(&mut digest.0[..H::OUTPUT_LEN])?;
        self.outer.update(&digest.0[..H::OUTPUT_LEN])?;
        self.outer.finish_into(output)
    }
    #[inline(always)]
    pub fn new(key: &[u8]) -> Result<Self, Error> {
        let mut mac = Self::unkeyed();
        mac.key(key)?;
        Ok(mac)
    }
    #[inline]
    pub fn update(&mut self, message: &[u8]) -> Result<(), Error> {
        self.inner.update(message)
    }
    #[inline(always)]
    pub fn finalize(mut self, output: &mut [u8]) -> Result<(), Error> {
        self.finish(output)
    }
    #[inline(always)]
    pub fn authenticate(key: &[u8], message: &[u8], output: &mut [u8]) -> Result<(), Error> {
        let mut mac = Self::unkeyed();
        mac.key(key)?;
        mac.update(message)?;
        mac.finish(output)
    }
    #[inline(always)]
    pub fn verify(mut self, tag: &[u8]) -> Result<(), Error> {
        if tag.len() != H::OUTPUT_LEN {
            return Err(Error::Length);
        }
        let mut expected = Secret([0u8; 64]);
        self.finish(&mut expected.0[..H::OUTPUT_LEN])?;
        if crate::ct::equal(&expected.0[..H::OUTPUT_LEN], tag) {
            Ok(())
        } else {
            Err(Error::Authentication)
        }
    }
}

/// An extracted HKDF pseudorandom key, erased on drop and never printable.
pub struct Hkdf<H: Digest> {
    prk: Secret<64>,
    hash: core::marker::PhantomData<H>,
}

impl<H: Digest> Hkdf<H> {
    #[inline]
    pub fn extract(salt: &[u8], input_key_material: &[u8]) -> Result<Self, Error> {
        let mut prk = Secret([0u8; 64]);
        Hmac::<H>::authenticate(salt, input_key_material, &mut prk.0[..H::OUTPUT_LEN])?;
        Ok(Self {
            prk,
            hash: core::marker::PhantomData,
        })
    }
    /// Imports a PRK from an existing protocol key schedule.
    pub fn from_prk(prk: &[u8]) -> Result<Self, Error> {
        if prk.len() != H::OUTPUT_LEN {
            return Err(Error::Length);
        }
        let mut key = Secret([0u8; 64]);
        key.0[..prk.len()].copy_from_slice(prk);
        Ok(Self {
            prk: key,
            hash: core::marker::PhantomData,
        })
    }
    #[inline]
    pub fn expand(&self, info: &[&[u8]], output: &mut [u8]) -> Result<(), Error> {
        if output.len() > 255 * H::OUTPUT_LEN {
            return Err(Error::Length);
        }
        let mut length = (H::BLOCK_LEN + H::OUTPUT_LEN + 1) as u128;
        for part in info {
            length = length
                .checked_add(part.len() as u128)
                .ok_or(Error::Length)?;
        }
        if length > H::MAX_BYTES {
            return Err(Error::Length);
        }
        let mut previous = Secret([0u8; 64]);
        let mut used = 0;
        for (i, chunk) in output.chunks_mut(H::OUTPUT_LEN).enumerate() {
            let mut mac = Hmac::<H>::unkeyed();
            mac.key(&self.prk.0[..H::OUTPUT_LEN])?;
            mac.update(&previous.0[..used])?;
            for part in info {
                mac.update(part)?;
            }
            mac.update(&[(i + 1) as u8])?;
            mac.finish(&mut previous.0[..H::OUTPUT_LEN])?;
            chunk.copy_from_slice(&previous.0[..chunk.len()]);
            used = H::OUTPUT_LEN;
        }
        Ok(())
    }
}
