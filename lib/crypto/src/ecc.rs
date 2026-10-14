//! Consumer-facing P-256 and P-384 types. SEC 1 public keys are validated
//! before arithmetic. ECDSA is verify-only. ECDH consumes its secret owner.
//! Generation considers four independent uniform candidates on a fixed schedule;
//! the only disclosed result is whether at least one candidate was valid.

macro_rules! curve_api {
    ($module:ident,$limbs:literal,$bytes:literal,$encoded:literal,$hash:ident) => {
        pub mod $module {
            use crate::{
                Error, Random,
                hash::$hash,
                montgomery::{Mod, V},
                nist::{Curve, Point},
                secret::Secret,
            };
            pub struct PublicKey {
                curve: Curve<$limbs>,
                point: Point<$limbs>,
                encoded: [u8; $encoded],
            }
            pub struct SecretKey {
                curve: Curve<$limbs>,
                scalar: V<$limbs>,
            }
            pub struct SharedSecret(Secret<$bytes>);
            impl SharedSecret {
                pub fn as_bytes(&self) -> &[u8; $bytes] {
                    &self.0.0
                }
            }
            impl PublicKey {
                pub fn from_sec1(input: &[u8]) -> Result<Self, Error> {
                    let curve = crate::nist::$module();
                    let point = curve.decode(input)?;
                    let encoded = input.try_into().map_err(|_| Error::Length)?;
                    Ok(Self {
                        curve,
                        point,
                        encoded,
                    })
                }
                pub fn as_sec1(&self) -> &[u8; $encoded] {
                    &self.encoded
                }
                pub fn verify(&self, message: &[u8], der_signature: &[u8]) -> Result<(), Error> {
                    self.verify_digest(&$hash::digest(message)?, der_signature)
                }
                pub fn verify_digest(
                    &self,
                    digest: &[u8; $bytes],
                    der_signature: &[u8],
                ) -> Result<(), Error> {
                    self.curve.verify(&self.point, digest, der_signature)
                }
            }
            impl SecretKey {
                pub fn from_bytes(bytes: [u8; $bytes]) -> Result<Self, Error> {
                    let bytes = Secret(bytes);
                    let curve = crate::nist::$module();
                    let scalar = curve.scalar(&bytes.0)?;
                    Ok(Self { curve, scalar })
                }
                pub fn generate(random: &mut impl Random) -> Result<Self, Error> {
                    let curve = crate::nist::$module();
                    let mut scalar = V::zero();
                    let mut selected = 0;
                    for _ in 0..4 {
                        let mut candidate = Secret([0; $bytes]);
                        random.fill(&mut candidate.0)?;
                        let candidate = Mod::from_be(&candidate.0);
                        let valid = curve.n.valid(&candidate) & (1 ^ candidate.eq(&V::zero()));
                        scalar = V::select(&scalar, &candidate, valid & (1 ^ selected));
                        selected |= valid;
                    }
                    if selected != 1 {
                        return Err(Error::Entropy);
                    }
                    Ok(Self { curve, scalar })
                }
                pub fn public_key(&self) -> PublicKey {
                    let point = self.curve.base().mul(&self.scalar, &self.curve);
                    let mut encoded = [0; $encoded];
                    point
                        .encode(&self.curve, &mut encoded)
                        .expect("nonzero prime-order scalar");
                    PublicKey::from_sec1(&encoded).expect("curve arithmetic invariant")
                }
                pub fn agree(self, peer: &PublicKey) -> Result<SharedSecret, Error> {
                    let mut out = Secret([0; $bytes]);
                    self.curve.agree(&self.scalar, &peer.point, &mut out.0)?;
                    Ok(SharedSecret(out))
                }
            }
        }
    };
}
curve_api!(p256, 4, 32, 65, Sha256);
curve_api!(p384, 6, 48, 97, Sha384);
