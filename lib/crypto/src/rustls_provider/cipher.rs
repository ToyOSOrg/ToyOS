use crate::aead::{OpeningKey, SealingKey};
use alloc::boxed::Box;
use rustls::{ConnectionTrafficSecrets, ContentType, Error, ProtocolVersion, crypto::cipher::*};

pub(super) enum Algorithm {
    Aes128,
    Aes256,
    ChaCha,
}
struct Seal {
    key: SealingKey,
    next: u64,
    version13: bool,
    explicit: bool,
}
struct Open {
    key: OpeningKey,
    iv: [u8; 12],
    version13: bool,
    explicit: bool,
}
impl Algorithm {
    fn key_bytes<'a>(&self, key: &'a AeadKey) -> &'a [u8] {
        &key.as_ref()[..Tls13AeadAlgorithm::key_len(self)]
    }
    fn sealing(&self, key: &AeadKey, iv: [u8; 12], version13: bool, explicit: bool) -> Box<Seal> {
        let storage = match self {
            Self::Aes128 | Self::Aes256 => {
                SealingKey::aes_storage(Tls13AeadAlgorithm::key_len(self), iv)
            }
            Self::ChaCha => SealingKey::chacha20_poly1305([0; 32], iv),
        };
        let mut out = Box::new(Seal {
            key: storage,
            next: 0,
            version13,
            explicit,
        });
        out.key.initialize(self.key_bytes(key));
        out
    }
    fn opening(&self, key: &AeadKey, iv: [u8; 12], version13: bool, explicit: bool) -> Box<Open> {
        let storage = match self {
            Self::Aes128 | Self::Aes256 => {
                OpeningKey::aes_storage(Tls13AeadAlgorithm::key_len(self))
            }
            Self::ChaCha => OpeningKey::chacha20_poly1305([0; 32]),
        };
        let mut out = Box::new(Open {
            key: storage,
            iv,
            version13,
            explicit,
        });
        out.key.initialize(self.key_bytes(key));
        out
    }
    fn secrets(&self, key: AeadKey, iv: Iv) -> ConnectionTrafficSecrets {
        match self {
            Self::Aes128 => ConnectionTrafficSecrets::Aes128Gcm { key, iv },
            Self::Aes256 => ConnectionTrafficSecrets::Aes256Gcm { key, iv },
            Self::ChaCha => ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv },
        }
    }
}
impl Tls13AeadAlgorithm for Algorithm {
    fn encrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageEncrypter> {
        self.sealing(&key, iv.as_ref().try_into().unwrap(), true, false)
    }
    fn decrypter(&self, key: AeadKey, iv: Iv) -> Box<dyn MessageDecrypter> {
        self.opening(&key, iv.as_ref().try_into().unwrap(), true, false)
    }
    fn key_len(&self) -> usize {
        if matches!(self, Self::Aes128) { 16 } else { 32 }
    }
    fn extract_keys(
        &self,
        key: AeadKey,
        iv: Iv,
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        Ok(self.secrets(key, iv))
    }
}
impl Tls12AeadAlgorithm for Algorithm {
    fn key_block_shape(&self) -> KeyBlockShape {
        let gcm = !matches!(self, Self::ChaCha);
        KeyBlockShape {
            enc_key_len: Tls13AeadAlgorithm::key_len(self),
            fixed_iv_len: if gcm { 4 } else { 12 },
            explicit_nonce_len: if gcm { 8 } else { 0 },
        }
    }
    fn encrypter(&self, key: AeadKey, iv: &[u8], extra: &[u8]) -> Box<dyn MessageEncrypter> {
        let mut nonce = [0; 12];
        nonce[..iv.len()].copy_from_slice(iv);
        let explicit = !matches!(self, Self::ChaCha);
        if explicit {
            nonce[4..].copy_from_slice(extra);
        }
        self.sealing(&key, nonce, false, explicit)
    }
    fn decrypter(&self, key: AeadKey, iv: &[u8]) -> Box<dyn MessageDecrypter> {
        let mut nonce = [0; 12];
        nonce[..iv.len()].copy_from_slice(iv);
        self.opening(&key, nonce, false, !matches!(self, Self::ChaCha))
    }
    fn extract_keys(
        &self,
        key: AeadKey,
        iv: &[u8],
        extra: &[u8],
    ) -> Result<ConnectionTrafficSecrets, UnsupportedOperationError> {
        let mut nonce = [0; 12];
        nonce[..iv.len()].copy_from_slice(iv);
        if !matches!(self, Self::ChaCha) {
            nonce[4..].copy_from_slice(extra);
        }
        Ok(self.secrets(key, Iv::from(nonce)))
    }
}
impl MessageEncrypter for Seal {
    fn encrypt(
        &mut self,
        msg: OutboundPlainMessage<'_>,
        seq: u64,
    ) -> Result<OutboundOpaqueMessage, Error> {
        // The protocol sequence must match the key owner's unrewindable sequence.
        if seq != self.next || seq == u64::MAX {
            return Err(Error::EncryptError);
        }
        if msg.payload.len() > 16384 {
            return Err(Error::EncryptError);
        }
        let mut payload =
            PrefixedPayload::with_capacity(self.encrypted_payload_len(msg.payload.len()));
        if self.explicit {
            payload.extend_from_slice(&[0; 8]);
        }
        payload.extend_from_chunks(&msg.payload);
        if self.version13 {
            payload.extend_from_slice(&[msg.typ.into()]);
        }
        let aad13 = make_tls13_aad(payload.as_ref().len() + 16);
        let aad12 = make_tls12_aad(seq, msg.typ, msg.version, msg.payload.len());
        let offset = if self.explicit { 8 } else { 0 };
        let (nonce, tag) = self
            .key
            .seal(
                if self.version13 { &aad13 } else { &aad12 },
                &mut payload.as_mut()[offset..],
            )
            .map_err(|_| Error::EncryptError)?;
        if self.explicit {
            payload.as_mut()[..8].copy_from_slice(&nonce[4..]);
        }
        payload.extend_from_slice(&tag);
        self.next += 1;
        Ok(OutboundOpaqueMessage::new(
            if self.version13 {
                ContentType::ApplicationData
            } else {
                msg.typ
            },
            if self.version13 {
                ProtocolVersion::TLSv1_2
            } else {
                msg.version
            },
            payload,
        ))
    }
    fn encrypted_payload_len(&self, len: usize) -> usize {
        len + 16 + usize::from(self.version13) + if self.explicit { 8 } else { 0 }
    }
}
impl MessageDecrypter for Open {
    fn decrypt<'a>(
        &mut self,
        mut msg: InboundOpaqueMessage<'a>,
        seq: u64,
    ) -> Result<InboundPlainMessage<'a>, Error> {
        let offset = if self.explicit { 8 } else { 0 };
        if msg.payload.len() < offset + 16 {
            return Err(Error::DecryptError);
        }
        let len = msg.payload.len() - 16;
        let mut nonce = self.iv;
        if self.explicit {
            nonce[4..].copy_from_slice(&msg.payload[..8]);
        } else {
            for (n, s) in nonce[4..].iter_mut().zip(seq.to_be_bytes()) {
                *n ^= s;
            }
        }
        let aad13 = make_tls13_aad(msg.payload.len());
        let aad12 = make_tls12_aad(seq, msg.typ, msg.version, len - offset);
        let (data, tag) = msg.payload.split_at_mut(len);
        self.key
            .open(
                &nonce,
                if self.version13 { &aad13 } else { &aad12 },
                &mut data[offset..],
                tag,
            )
            .map_err(|_| Error::DecryptError)?;
        msg.payload.truncate(len);
        if self.version13 {
            msg.into_tls13_unpadded_message()
        } else {
            if len - offset > 16384 {
                return Err(Error::PeerSentOversizedRecord);
            }
            Ok(msg.into_plain_message_range(offset..len))
        }
    }
}
