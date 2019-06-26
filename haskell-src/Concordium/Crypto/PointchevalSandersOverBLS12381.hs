{-# LANGUAGE GeneralizedNewtypeDeriving #-}
{-# LANGUAGE TemplateHaskell #-}
{-# LANGUAGE DerivingVia #-}
module Concordium.Crypto.PointchevalSandersOverBLS12381 where

import Concordium.Crypto.PointchevalSanders.TH
import Foreign.C.Types

-- This import is needed because otherwise the FBSHex data constructor is hidden
-- and deriving via does not see that the representations of various types are
-- the same.
import Concordium.Crypto.ByteStringHelpers


mkPointChevalScheme Parameters{
  cDeriveCommitmentKey = "commitment_key_bls12_381",
  cGenerateSecretKey = "generate_secret_key_bls12_381",
  cDerivePublicKey = "public_key_bls12_381",
  cSignKnownMessage = "sign_known_message_bls12_381",
  cSignUnknownMessage = "sign_unknown_message_bls12_381",
  cVerifySignatureKnown = "verify_bls12_381",
  cRetrieveSignature = "retrieve_sig_bls12_381",
  cCommitWithPublicKey = "commit_with_pk_bls12_381",
  cRandomValuesName = "random_values_bls12_381_g1_proj",
  groupG1ElementSize = 48,
  groupG2ElementSize = 96,
  fieldElementSize = 32
  }
