#![allow(unused_macros)]
#![allow(unused_imports)]
#![allow(dead_code)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![cfg_attr(test, allow(unused_variables, unused_assignments))]
#![allow(clippy::assertions_on_constants)]

#[macro_use(o, slog_log, slog_trace, slog_debug, slog_info, slog_warn, slog_error)]
extern crate slog;

#[macro_use]
extern crate serde_derive;

#[cfg(unix)]
extern crate nix;

#[cfg(windows)]
extern crate winapi;

#[macro_use]
pub mod util;

#[macro_use]
pub mod codec;

pub mod types;

pub mod address;

pub mod deps_common;

pub mod bitvec;

use crate::types::chainstate::{BlockHeaderHash, BurnchainHeaderHash, SortitionId, StacksBlockId};

pub mod consts {
    use crate::types::chainstate::{BlockHeaderHash, ConsensusHash};

    pub const TOKEN_TRANSFER_MEMO_LENGTH: usize = 34; // same as it is in Stacks v1

    pub const BITCOIN_REGTEST_FIRST_BLOCK_HEIGHT: u64 = 0;
    pub const BITCOIN_REGTEST_FIRST_BLOCK_TIMESTAMP: u32 = 0;
    pub const BITCOIN_REGTEST_FIRST_BLOCK_HASH: &str =
        "0000000000000000000000000000000000000000000000000000000000000000";

    pub const FIRST_STACKS_BLOCK_HASH: BlockHeaderHash = BlockHeaderHash([0u8; 32]);

    pub const FIRST_BURNCHAIN_CONSENSUS_HASH: ConsensusHash = ConsensusHash([0u8; 20]);

    pub const CHAIN_ID_MAINNET: u32 = 0x00000001;
    pub const CHAIN_ID_TESTNET: u32 = 0x80000000;

    #[cfg(any(test, feature = "testing"))]
    pub const MINER_REWARD_MATURITY: u64 = 2; // small for testing purposes

    #[cfg(not(any(test, feature = "testing")))]
    pub const MINER_REWARD_MATURITY: u64 = 100;

    pub const STACKS_EPOCH_MAX: u64 = i64::MAX as u64;

    /// The number of StackerDB slots each signing key needs
    ///  to use to participate in DKG and block validation signing.
    pub const SIGNER_SLOTS_PER_USER: u32 = 13;
}
