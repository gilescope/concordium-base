use crate::errors::AggregateSigError;
use curve_arithmetic::{Curve, Pairing};
use ff::Field;
use rand::Rng;
use rayon::{iter::*, join};
use sha2::{Digest, Sha512};
use std::{cmp::Ordering, io::Cursor};

pub const PUBLIC_KEY_SIZE: usize = 96;
pub const SECRET_KEY_SIZE: usize = 32;
pub const SIGNATURE_SIZE: usize = 48;

#[derive(Debug)]
pub struct SecretKey<P: Pairing>(P::ScalarField);

impl<P: Pairing> SecretKey<P> {
    pub fn generate<R: Rng>(rng: &mut R) -> SecretKey<P> { SecretKey(P::generate_scalar(rng)) }

    pub fn from_bytes(b: &mut Cursor<&[u8]>) -> Result<SecretKey<P>, AggregateSigError> {
        let s = P::bytes_to_scalar(b)?;
        Ok(SecretKey(s))
    }

    pub fn to_bytes(&self) -> Box<[u8]> { P::scalar_to_bytes(&self.0) }

    // Sign a message using the SecretKey
    pub fn sign(&self, m: &[u8]) -> Signature<P> {
        let g1_hash = P::G_1::hash_to_group(m);
        let signature = g1_hash.mul_by_scalar(&self.0);
        Signature(signature)
    }

    // Returns the size of signature as a bytearray
    pub fn len() -> usize { P::SCALAR_LENGTH }
}

impl<P: Pairing> Clone for SecretKey<P> {
    fn clone(&self) -> Self { SecretKey(self.0) }
}

impl<P: Pairing> Copy for SecretKey<P> {}

#[derive(Debug)]
pub struct PublicKey<P: Pairing>(P::G_2);

impl<P: Pairing> PublicKey<P> {
    pub fn from_secret(sk: SecretKey<P>) -> PublicKey<P> {
        PublicKey(P::G_2::one_point().mul_by_scalar(&sk.0))
    }

    // Verifies a single message and signature pair using this PublicKey
    pub fn verify(&self, m: &[u8], signature: Signature<P>) -> bool {
        let g1_hash = P::G_1::hash_to_group(m);
        // compute pairings in parallel
        let (pair1, pair2): (P::TargetField, P::TargetField) = join(
            || P::pair(signature.0, P::G_2::one_point()),
            || P::pair(g1_hash, self.0),
        );
        pair1 == pair2
    }

    pub fn from_bytes(b: &mut Cursor<&[u8]>) -> Result<PublicKey<P>, AggregateSigError> {
        let point = P::G_2::bytes_to_curve(b)?;
        Ok(PublicKey(point))
    }

    pub fn to_bytes(&self) -> Box<[u8]> { P::G_2::curve_to_bytes(&self.0) }

    // Returns the size of signature as a bytearray
    pub fn len() -> usize { P::G_2::GROUP_ELEMENT_LENGTH }
}

impl<P: Pairing> Clone for PublicKey<P> {
    fn clone(&self) -> Self { PublicKey(self.0) }
}

impl<P: Pairing> Copy for PublicKey<P> {}

#[derive(Debug)]
pub struct Signature<P: Pairing>(P::G_1);

impl<P: Pairing> Signature<P> {
    // Aggregates this signatures with the given signature.
    pub fn aggregate(&self, to_aggregate: Signature<P>) -> Signature<P> {
        Signature(self.0.plus_point(&to_aggregate.0))
    }

    pub fn from_bytes(b: &mut Cursor<&[u8]>) -> Result<Signature<P>, AggregateSigError> {
        let point = P::G_1::bytes_to_curve(b)?;
        Ok(Signature(point))
    }

    pub fn to_bytes(&self) -> Box<[u8]> { P::G_1::curve_to_bytes(&self.0) }

    // Returns the size of signature as a bytearray
    pub fn len() -> usize { P::G_1::GROUP_ELEMENT_LENGTH }
}

impl<P: Pairing> Clone for Signature<P> {
    fn clone(&self) -> Self { Signature(self.0) }
}

impl<P: Pairing> Copy for Signature<P> {}

pub fn verify_aggregate_sig<P: Pairing>(
    m_pk_pairs: &[(&[u8], PublicKey<P>)],
    signature: Signature<P>,
) -> bool {
    // Check for duplicates in messages. Reject if any
    if has_duplicates(m_pk_pairs.iter().map(|x| x.0)) {
        return false;
    }

    let product = m_pk_pairs
        .par_iter()
        .fold(<P::TargetField as Field>::zero, |_sum, x| {
            let (m, pk) = x;
            let g1_hash = P::G_1::hash_to_group(m);
            P::pair(g1_hash, pk.0)
        })
        .reduce(<P::TargetField as Field>::one, |prod, x| {
            let mut p = prod;
            p.mul_assign(&x);
            p
        });

    P::pair(signature.0, P::G_2::one_point()) == product
}

pub fn verify_aggregate_sig_trusted_keys<P: Pairing>(
    m: &[u8],
    pks: &[PublicKey<P>],
    signature: Signature<P>,
) -> bool {
    let sum = pks
        .par_iter()
        .fold(P::G_2::zero_point, |_sum, x| x.0)
        .reduce(P::G_2::zero_point, |sum, x| sum.plus_point(&x));

    // compute pairings in parallel
    let (pair1, pair2): (P::TargetField, P::TargetField) = join(
        || P::pair(signature.0, P::G_2::one_point()),
        || P::pair(P::G_1::hash_to_group(m), sum),
    );
    pair1 == pair2
}

// A wrapper for sha512 hashes. Only use is to have the Ord trait on [u8; 64]
// for sorting an array of hashes. See has_duplicates below
struct Hash([u8; 64]);

impl PartialEq for Hash {
    fn eq(&self, other: &Self) -> bool { self.0[0..63] == other.0[0..63] }
}

impl Eq for Hash {}

impl PartialOrd for Hash {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(&other)) }
}

impl Ord for Hash {
    fn cmp(&self, other: &Self) -> Ordering { self.0[0..63].cmp(&other.0[0..63]) }
}

// Checks for duplicates in a list of messages
// This is not very efficient - the sorting algorithm can exit as soon as it
// encounters an equality and report that a duplicate indeed exists.
// Consider building hashmap or Btree and exit as soon as a duplicate is seen
fn has_duplicates<'a>(messages_iter: impl Iterator<Item = &'a [u8]>) -> bool {
    let mut message_hashes: Vec<Hash> = messages_iter
        .map(|x| {
            let h = hash_message(x);
            Hash(h)
        })
        .collect();
    message_hashes.sort();
    for i in 1..message_hashes.len() {
        if message_hashes[i - 1] == message_hashes[i] {
            return true;
        }
    }
    false
}

// hashes a message using Sha512
fn hash_message(m: &[u8]) -> [u8; 64] {
    let mut h = Sha512::new();
    let mut hash: [u8; 64] = [0u8; 64];
    h.input(m);
    hash.copy_from_slice(h.result().as_slice());
    hash
}

#[cfg(test)]
mod test {
    use super::*;
    use pairing::bls12_381::Bls12;
    use rand::{Rng, SeedableRng, StdRng};

    const SIGNERS: usize = 10;
    const TEST_ITERATIONS: usize = 1000;

    // returns a pair of lists (sks, pks), such that sks[i] and pks[i] are
    // corresponding secret and public key
    fn get_sks_pks<P: Pairing>(
        amt: usize,
        rng: &mut StdRng,
    ) -> (Vec<SecretKey<P>>, Vec<PublicKey<P>>) {
        let sks: Vec<SecretKey<P>> = (0..amt).map(|_| SecretKey::<P>::generate(rng)).collect();

        let pks: Vec<PublicKey<P>> = sks
            .iter()
            .map(|x| PublicKey::<P>::from_secret(*x))
            .collect();
        (sks, pks)
    }

    // returns a list of random bytes (of length 32)
    fn get_random_messages<R: Rng>(amt: usize, rng: &mut R) -> Vec<[u8; 32]> {
        (0..amt).map(|_| rng.gen::<[u8; 32]>()).collect()
    }

    #[test]
    fn test_sign_and_verify() {
        let seed: &[_] = &[1];
        let mut rng: StdRng = SeedableRng::from_seed(seed);

        for _ in 0..TEST_ITERATIONS {
            let sk = SecretKey::<Bls12>::generate(&mut rng);
            let pk = PublicKey::from_secret(sk);

            // should verify correctly
            let m = rng.gen::<[u8; 32]>();
            let signature = sk.sign(&m);
            assert!(pk.verify(&m, signature));

            // should not verify!
            let signature = sk.sign(&m);
            let sk2 = SecretKey::<Bls12>::generate(&mut rng);
            let pk2 = PublicKey::from_secret(sk2);
            assert!(!pk2.verify(&m, signature))
        }
    }

    macro_rules! aggregate_sigs {
        ($messages:expr, $sks:expr) => {{
            let mut sig = $sks[0].sign(&$messages[0]);
            for i in 1..$sks.len() {
                let my_sig = $sks[i].sign(&$messages[i]);
                sig = sig.aggregate(my_sig);
            }
            sig
        }};
    }

    #[test]
    fn test_verify_aggregate_sig() {
        let seed: &[_] = &[1];
        let mut rng: StdRng = SeedableRng::from_seed(seed);

        let (sks, pks) = get_sks_pks(SIGNERS, &mut rng);

        for _ in 0..TEST_ITERATIONS {
            let ms = get_random_messages(SIGNERS, &mut rng);
            let sig = aggregate_sigs!(ms, sks);

            let mut m_pk_pairs: Vec<(&[u8], PublicKey<Bls12>)> = Vec::new();
            for i in 0..SIGNERS {
                m_pk_pairs.push((&ms[i], pks[i].clone()));
            }

            // signature should verify
            assert!(verify_aggregate_sig(&m_pk_pairs, sig));

            let (m_, pk_) = m_pk_pairs.pop().unwrap();
            let new_pk = PublicKey::<Bls12>::from_secret(SecretKey::<Bls12>::generate(&mut rng));
            m_pk_pairs.push((m_, new_pk));

            // altering a public key should make verification fail
            assert!(!verify_aggregate_sig(&m_pk_pairs, sig));

            let new_m: [u8; 32] = rng.gen::<[u8; 32]>();
            m_pk_pairs.pop();
            m_pk_pairs.push((&new_m, pk_));

            // altering a message should make verification fail
            assert!(!verify_aggregate_sig(&m_pk_pairs, sig));
        }
    }

    #[test]
    fn test_verify_aggregate_sig_trusted_keys() {
        let seed: &[_] = &[1];
        let mut rng: StdRng = SeedableRng::from_seed(seed);
        for _ in 0..TEST_ITERATIONS {
            let (sks, pks) = get_sks_pks(SIGNERS, &mut rng);
            let m: [u8; 32] = rng.gen::<[u8; 32]>();
            let sigs: Vec<Signature<Bls12>> = sks.iter().map(|sk| sk.sign(&m)).collect();
            let mut agg_sig = sigs[0].clone();
            sigs.iter().skip(1).for_each(|x| {
                agg_sig = agg_sig.aggregate(*x);
            });

            assert!(verify_aggregate_sig_trusted_keys(&m, &pks, agg_sig));

            // test changing message makes verification fails
            let m_alt: [u8; 32] = rng.gen::<[u8; 32]>();
            assert!(!verify_aggregate_sig_trusted_keys(&m_alt, &pks, agg_sig));

            // test that adding or removing a public key makes verification fail
            let mut pks_alt = pks.clone();
            pks_alt.push(PublicKey::<Bls12>::from_secret(
                SecretKey::<Bls12>::generate(&mut rng),
            ));
            assert!(!verify_aggregate_sig_trusted_keys(&m, &pks_alt, agg_sig));

            // test that removing a public key makes verification fail
            pks_alt.pop();
            pks_alt.pop();
            assert!(!verify_aggregate_sig_trusted_keys(&m, &pks_alt, agg_sig));

            let agg_sig_alt = Signature(<Bls12 as Pairing>::G_1::generate(&mut rng));
            assert!(!verify_aggregate_sig_trusted_keys(&m, &pks, agg_sig_alt));
        }
    }

    #[test]
    fn test_has_duplicates() {
        let seed: &[_] = &[1];
        let mut rng: StdRng = SeedableRng::from_seed(seed);

        for _ in 0..TEST_ITERATIONS {
            let mut ms: Vec<[u8; 8]> = (0..SIGNERS).map(|x| x.to_le_bytes()).collect();

            // Make a duplication in the messages
            let random_idx1: usize = rng.gen_range(0, SIGNERS);
            let mut random_idx2: usize = rng.gen_range(0, SIGNERS);
            while random_idx1 == random_idx2 {
                random_idx2 = rng.gen_range(0, SIGNERS);
            }
            ms[random_idx1] = ms[random_idx2];

            let iter = (0..SIGNERS).map(|i| -> &[u8] { &ms[i] });
            let result = has_duplicates(iter);
            assert!(result);
        }
    }

    #[test]
    fn test_to_from_bytes_identity() {
        let seed: &[_] = &[1];
        let mut rng: StdRng = SeedableRng::from_seed(seed);

        for _ in 0..1000 {
            let m = rng.gen::<[u8; 32]>();
            let sk = SecretKey::<Bls12>::generate(&mut rng);
            let pk = PublicKey::<Bls12>::from_secret(sk);
            let sig = sk.sign(&m);
            let sk_bytes = sk.to_bytes();
            let pk_bytes = pk.to_bytes();
            let sig_bytes = sig.to_bytes();
            let sk_from_bytes =
                SecretKey::<Bls12>::from_bytes(&mut Cursor::new(&sk_bytes)).unwrap();
            let pk_from_bytes =
                PublicKey::<Bls12>::from_bytes(&mut Cursor::new(&pk_bytes)).unwrap();
            let sig_from_bytes =
                Signature::<Bls12>::from_bytes(&mut Cursor::new(&sig_bytes)).unwrap();

            assert_eq!(sig.0, sig_from_bytes.0);
            assert_eq!(sk.0, sk_from_bytes.0);
            assert_eq!(pk.0, pk_from_bytes.0);
            assert!(pk.verify(&m, sig_from_bytes));
            assert!(pk_from_bytes.verify(&m, sig_from_bytes));
        }
    }

    #[test]
    fn test_to_bytes_correct_length() {
        let seed: &[_] = &[1];
        let mut rng: StdRng = SeedableRng::from_seed(seed);

        for _ in 0..1000 {
            let m = rng.gen::<[u8; 32]>();
            let sk = SecretKey::<Bls12>::generate(&mut rng);
            let pk = PublicKey::<Bls12>::from_secret(sk);
            let sig = sk.sign(&m);

            let sk_bytes = sk.to_bytes();
            let pk_bytes = pk.to_bytes();
            let sig_bytes = sig.to_bytes();

            assert_eq!(sk_bytes.len(), SecretKey::<Bls12>::len());
            assert_eq!(pk_bytes.len(), PublicKey::<Bls12>::len());
            assert_eq!(sig_bytes.len(), Signature::<Bls12>::len());
        }
    }
}
