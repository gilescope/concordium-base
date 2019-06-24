// -*- mode: rust; -*-
//
// Authors:
// - bm@concordium.com

//! A secret key

#[cfg(feature = "serde")]
use serde::de::Error as SerdeError;
#[cfg(feature = "serde")]
use serde::de::Visitor;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "serde")]
use serde::{Deserializer, Serializer};

use crate::{
    errors::{
        InternalError::{FieldDecodingError, SecretKeyLengthError},
        *,
    },
    known_message::*,
    signature::*,
    unknown_message::*,
};
use curve_arithmetic::curve_arithmetic::*;
use pairing::Field;

use rand::*;

/// A secret key
#[derive(Debug)]
pub struct SecretKey<C: Pairing>(pub(crate) Vec<C::ScalarField>, pub(crate) C::ScalarField);

impl<C: Pairing> PartialEq for SecretKey<C> {
    fn eq(&self, other: &Self) -> bool { self.0 == other.0 && self.1 == other.1 }
}

impl<C: Pairing> Eq for SecretKey<C> {}

impl<C: Pairing> SecretKey<C> {
    // turn secret key vector into a byte array
    #[inline]
    pub fn to_bytes(&self) -> Box<[u8]> {
        let vs = &self.0;
        let u = &self.1;
        let mut bytes: Vec<u8> = Vec::new();
        for v in vs.iter() {
            bytes.extend_from_slice(&*Self::value_to_bytes(&v));
        }
        bytes.extend_from_slice(&*Self::value_to_bytes(u));
        bytes.into_boxed_slice()
    }

    #[inline]
    pub fn value_to_bytes(scalar: &C::ScalarField) -> Box<[u8]> { C::scalar_to_bytes(scalar) }

    /// Construct a secret key vec from a slice of bytes.
    ///
    /// A `Result` whose okay value is a secret key vec  or whose error value
    /// is an `SignatureError` wrapping the internal error that occurred.
    #[inline]
    pub fn from_bytes(bytes: &[u8]) -> Result<SecretKey<C>, SignatureError> {
        let l = bytes.len();
        if l == 0 || l < C::SCALAR_LENGTH || l % C::SCALAR_LENGTH != 0 {
            return Err(SignatureError(SecretKeyLengthError));
        }
        let vlen = (l / C::SCALAR_LENGTH) - 1;
        let mut vs: Vec<C::ScalarField> = Vec::new();
        for i in 0..vlen {
            let j = i * C::SCALAR_LENGTH;
            let k = j + C::SCALAR_LENGTH;
            match C::bytes_to_scalar(&bytes[j..k]) {
                Err(_) => return Err(SignatureError(FieldDecodingError)),
                Ok(fr) => vs.push(fr),
            }
        }
        match C::bytes_to_scalar(&bytes[(l - C::SCALAR_LENGTH)..]) {
            Err(_) => Err(SignatureError(FieldDecodingError)),
            Ok(fr) => Ok(SecretKey(vs, fr)),
        }
    }

    /// Generate a secret key  from a `csprng`.
    pub fn generate<T>(n: usize, csprng: &mut T) -> SecretKey<C>
    where
        T: Rng, {
        let mut vs: Vec<C::ScalarField> = Vec::new();
        for _i in 0..n {
            vs.push(C::generate_scalar(csprng));
        }

        SecretKey(vs, C::generate_scalar(csprng))
    }

    pub fn sign_known_message<T>(
        &self,
        message: &KnownMessage<C>,
        csprng: &mut T,
    ) -> Result<Signature<C>, SignatureError>
    where
        T: Rng, {
        let ys = &self.0;
        let ms = &message.0;
        if ms.len() > ys.len() {
            return Err(SignatureError(SecretKeyLengthError));
        }

        let mut z =
            ms.iter()
                .zip(ys.iter())
                .fold(<C::ScalarField as Field>::zero(), |mut acc, (m, y)| {
                    let mut r = *m;
                    r.mul_assign(y);
                    acc.add_assign(&r);
                    acc
                });
        z.add_assign(&self.1);
        let h = C::G_1::one_point().mul_by_scalar(&C::generate_scalar(csprng));

        Ok(Signature(h, h.mul_by_scalar(&z)))
    }

    pub fn sign_unknown_message<T>(
        &self,
        message: &UnknownMessage<C>,
        csprng: &mut T,
    ) -> Result<Signature<C>, SignatureError>
    where
        T: Rng, {
        let sk = C::G_1::one_point().mul_by_scalar(&self.1);
        let r = C::generate_scalar(csprng);
        let a = C::G_1::one_point().mul_by_scalar(&r);
        let m = message.0;
        let xmr = sk.plus_point(&m).mul_by_scalar(&r);
        Ok(Signature(a, xmr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pairing::bls12_381::Bls12;

    macro_rules! macro_test_secret_key_to_byte_conversion {
        ($function_name:ident, $pairing_type:path) => {
            #[test]
            pub fn $function_name() {
                let mut csprng = thread_rng();
                for i in 0..20 {
                    let val = SecretKey::<$pairing_type>::generate(i, &mut csprng);
                    let res_val2 = SecretKey::<$pairing_type>::from_bytes(&*val.to_bytes());
                    assert!(res_val2.is_ok());
                    let val2 = res_val2.unwrap();
                    assert_eq!(val2, val);
                }
            }
        };
    }

    macro_test_secret_key_to_byte_conversion!(secret_key_to_byte_conversion_bls12_381, Bls12);
}
