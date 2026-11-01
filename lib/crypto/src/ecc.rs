//! Consumer-facing P-256 and P-384 types. SEC 1 public keys are validated
//! before arithmetic. ECDSA is verify-only. ECDH consumes its secret owner.
//! Generation considers four independent uniform candidates on a fixed schedule;
//! the only disclosed result is whether at least one candidate was valid.

macro_rules! curve_api {
    ($module:ident, $curve:ident, $limbs:literal, $bytes:literal, $encoded:literal, $hash:ident) => {
        pub mod $module {
            use crate::{
                Error, Random,
                hash::$hash,
                nist::{$curve, Curve, Point, Scalar},
                secret::Secret,
            };
            pub struct PublicKey {
                point: Point<$curve, $limbs>,
                encoded: [u8; $encoded],
            }
            pub struct SecretKey(Scalar<$curve, $limbs>);
            pub struct SharedSecret(Secret<$bytes>);
            impl SharedSecret {
                pub fn as_bytes(&self) -> &[u8; $bytes] {
                    &self.0.0
                }
            }
            impl PublicKey {
                pub fn from_sec1(input: &[u8]) -> Result<Self, Error> {
                    let point = Point::decode(input)?;
                    let encoded = input.try_into().map_err(|_| Error::Length)?;
                    Ok(Self { point, encoded })
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
                    self.point.verify(digest, der_signature)
                }
            }
            impl SecretKey {
                pub fn from_bytes(bytes: [u8; $bytes]) -> Result<Self, Error> {
                    let bytes = Secret(bytes);
                    Ok(Self(Scalar::from_bytes(&bytes.0)?))
                }
                pub fn generate(random: &mut impl Random) -> Result<Self, Error> {
                    let mut candidates = Secret([0; 4 * $bytes]);
                    random.fill(&mut candidates.0)?;
                    Ok(Self(Scalar::first_valid(&candidates.0)?))
                }
                pub fn public_key(&self) -> PublicKey {
                    // The base point's multiple by a nonzero scalar below n
                    // is finite and on the curve.
                    let point = <$curve as Curve<$limbs>>::base_mul(&self.0);
                    let mut encoded = [0; $encoded];
                    point.encode(&mut encoded).expect("nonzero prime-order scalar");
                    PublicKey { point, encoded }
                }
                pub fn agree(self, peer: &PublicKey) -> Result<SharedSecret, Error> {
                    let mut out = Secret([0; $bytes]);
                    peer.point.agree(&self.0, &mut out.0);
                    Ok(SharedSecret(out))
                }
            }
        }
    };
}
curve_api!(p256, P256, 4, 32, 65, Sha256);
curve_api!(p384, P384, 6, 48, 97, Sha384);
