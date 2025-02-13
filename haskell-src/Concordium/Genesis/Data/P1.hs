-- |This module defines the genesis data fromat for the 'P1' protocol version.
module Concordium.Genesis.Data.P1 where

import Data.Serialize

import Concordium.Common.Version
import qualified Concordium.Crypto.SHA256 as Hash
import Concordium.Genesis.Data.Base
import Concordium.Types
import Concordium.Genesis.Parameters

-- |Genesis data for the P1 protocol version.
-- Two types of genesis data are supported.
--
-- * 'GDP1Initial' represents an initial genesis block.
--   It specifies how the initial state should be configured.
--
-- * 'GDP1Regenesis' represents a reset of the protocol with
--   a new genesis block.  This includes the full serialized
--   block state to use from this point forward.
--
-- The serialization of the block state may not be unique, so
-- only the hash of it is used in defining the hash of the
-- genesis block.
--
-- The relationship between the new state and the state of the
-- terminal block of the old chain should be defined by the
-- chain update mechanism used.
--
-- To the extent that the 'CoreGenesisParameters' are represented
-- in the block state, they should agree. (This is probably only
-- the epoch length.)
--
-- Note that the invariants regarding the 'genesisNewState' are
-- soft: deserialization does not check them, or even that the
-- serialization is valid.
data GenesisDataP1
    = -- |An initial genesis block.
      GDP1Initial
        { -- |The immutable genesis parameters.
          genesisCore :: !CoreGenesisParameters,
          -- |The blueprint for the initial state at genesis.
          genesisInitialState :: !GenesisState
        }
    | -- |A re-genesis block.
      GDP1Regenesis
        { genesisRegenesis :: !RegenesisData }
    deriving (Eq, Show)

_core :: GenesisDataP1 -> CoreGenesisParameters
_core GDP1Initial{..} = genesisCore
_core GDP1Regenesis{genesisRegenesis=RegenesisData{..}} = genesisCore

instance BasicGenesisData GenesisDataP1 where
    gdGenesisTime = genesisTime . _core
    {-# INLINE gdGenesisTime #-}
    gdSlotDuration = genesisSlotDuration . _core
    {-# INLINE gdSlotDuration #-}
    gdMaxBlockEnergy = genesisMaxBlockEnergy . _core
    {-# INLINE gdMaxBlockEnergy #-}
    gdFinalizationParameters = genesisFinalizationParameters . _core
    {-# INLINE gdFinalizationParameters #-}
    gdEpochLength = genesisEpochLength . _core
    {-# INLINE gdEpochLength #-}

-- |Deserialize genesis data in the V3 format.
getGenesisDataV3 :: Get GenesisDataP1
getGenesisDataV3 =
    getWord8 >>= \case
        0 -> do
            genesisCore <- get
            genesisInitialState <- get
            return GDP1Initial{..}
        1 -> GDP1Regenesis <$> getRegenesisData
        _ -> fail "Unrecognised genesis data type"

-- |Serialize genesis data in the V3 format.
putGenesisDataV3 :: Putter GenesisDataP1
putGenesisDataV3 GDP1Initial{..} = do
    putWord8 0
    put genesisCore
    put genesisInitialState
putGenesisDataV3 GDP1Regenesis{..} = do
    putWord8 1
    putRegenesisData genesisRegenesis

-- |Deserialize genesis data with a version tag.
getVersionedGenesisData :: Get GenesisDataP1
getVersionedGenesisData =
    getVersion >>= \case
        3 -> getGenesisDataV3
        n -> fail $ "Unsupported genesis data version: " ++ show n

-- |Serialize genesis data with a version tag.
-- This will use the V3 format.
putVersionedGenesisData :: Putter GenesisDataP1
putVersionedGenesisData gd = do
    putVersion 3
    putGenesisDataV3 gd

parametersToGenesisData :: GenesisParameters -> GenesisDataP1
parametersToGenesisData = uncurry GDP1Initial . parametersToState

-- |Compute the block hash of the genesis block with the given genesis data.
-- Every block hash is derived from a message that begins with the block slot,
-- which is 0 for genesis blocks.  For the genesis block, as of 'P1', we include
-- a signifier of the protocol version next.
--
-- Note, for regenesis blocks, the state is only represented by its hash.
genesisBlockHash :: GenesisDataP1 -> BlockHash
genesisBlockHash GDP1Initial{..} = BlockHash . Hash.hashLazy . runPutLazy $ do
    put genesisSlot
    put P1
    putWord8 0 -- Initial
    put genesisCore
    put genesisInitialState
genesisBlockHash GDP1Regenesis{genesisRegenesis=RegenesisData{..}} = BlockHash . Hash.hashLazy . runPutLazy $ do
    put genesisSlot
    put P1
    putWord8 1 -- Regenesis
    -- NB: 'putRegenesisData' is not used since the state serialization does not go into computing the hash.
    -- Only the state hash is used.
    put genesisCore
    put genesisFirstGenesis
    put genesisPreviousGenesis
    put genesisTerminalBlock
    put genesisStateHash
