// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2024 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
use std::io::{self, Read};
use std::net::SocketAddr;
use std::path::PathBuf;

use blockstack_lib::chainstate::stacks::address::PoxAddress;
use blockstack_lib::util_lib::signed_structured_data::pox4::Pox4SignatureTopic;
use clap::{ArgAction, Parser, ValueEnum};
use clarity::vm::types::QualifiedContractIdentifier;
use stacks_common::address::{
    b58, AddressHashMode, C32_ADDRESS_VERSION_MAINNET_MULTISIG,
    C32_ADDRESS_VERSION_MAINNET_SINGLESIG, C32_ADDRESS_VERSION_TESTNET_MULTISIG,
    C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
};
use stacks_common::types::chainstate::StacksPrivateKey;

use crate::config::Network;

extern crate alloc;

#[derive(Parser, Debug)]
#[command(author, version, about)]
/// The CLI arguments for the stacks signer
pub struct Cli {
    /// Subcommand action to take
    #[command(subcommand)]
    pub command: Command,
}

/// Subcommands for the stacks signer binary
#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Get a chunk from the stacker-db instance
    GetChunk(GetChunkArgs),
    /// Get the latest chunk from the stacker-db instance
    GetLatestChunk(GetLatestChunkArgs),
    /// List chunks from the stacker-db instance
    ListChunks(StackerDBArgs),
    /// Upload a chunk to the stacker-db instance
    PutChunk(PutChunkArgs),
    /// Run DKG and sign the message through the stacker-db instance
    DkgSign(SignArgs),
    /// Sign the message through the stacker-db instance
    Sign(SignArgs),
    /// Run a DKG round through the stacker-db instance
    Dkg(RunDkgArgs),
    /// Run the signer, waiting for events from the stacker-db instance
    Run(RunSignerArgs),
    /// Generate necessary files for running a collection of signers
    GenerateFiles(GenerateFilesArgs),
    /// Generate a signature for Stacking transactions
    GenerateStackingSignature(GenerateStackingSignatureArgs),
    /// Check a configuration file and output config information
    CheckConfig(RunSignerArgs),
}

/// Basic arguments for all cyrptographic and stacker-db functionality
#[derive(Parser, Debug, Clone)]
pub struct StackerDBArgs {
    /// The Stacks node to connect to
    #[arg(long)]
    pub host: String,
    /// The stacker-db contract to use. Must be in the format of "STACKS_ADDRESS.CONTRACT_NAME"
    #[arg(short, long, value_parser = parse_contract)]
    pub contract: QualifiedContractIdentifier,
}

/// Arguments for the get-chunk command
#[derive(Parser, Debug, Clone)]
pub struct GetChunkArgs {
    /// The base arguments
    #[clap(flatten)]
    pub db_args: StackerDBArgs,
    /// The slot ID to get
    #[arg(long)]
    pub slot_id: u32,
    /// The slot version to get
    #[arg(long)]
    pub slot_version: u32,
}

/// Arguments for the get-latest-chunk command
#[derive(Parser, Debug, Clone)]
pub struct GetLatestChunkArgs {
    /// The base arguments
    #[clap(flatten)]
    pub db_args: StackerDBArgs,
    /// The slot ID to get
    #[arg(long)]
    pub slot_id: u32,
}

#[derive(Parser, Debug, Clone)]
/// Arguments for the put-chunk command
pub struct PutChunkArgs {
    /// The base arguments
    #[clap(flatten)]
    pub db_args: StackerDBArgs,
    /// The Stacks private key to use in hexademical format
    #[arg(short, long, value_parser = parse_private_key)]
    pub private_key: StacksPrivateKey,
    /// The slot ID to get
    #[arg(long)]
    pub slot_id: u32,
    /// The slot version to get
    #[arg(long)]
    pub slot_version: u32,
    /// The data to upload
    #[arg(required = false, value_parser = parse_data)]
    // Note this weirdness is due to https://github.com/clap-rs/clap/discussions/4695
    // Need to specify the long name here due to invalid parsing in Clap which looks at the NAME rather than the TYPE which causes issues in how it handles Vec's.
    pub data: alloc::vec::Vec<u8>,
}

#[derive(Parser, Debug, Clone)]
/// Arguments for the dkg-sign and sign command
pub struct SignArgs {
    /// Path to config file
    #[arg(long, short, value_name = "FILE")]
    pub config: PathBuf,
    /// The reward cycle the signer is registered for and wants to sign for
    /// Note: this must be the current reward cycle of the node
    #[arg(long, short)]
    pub reward_cycle: u64,
    /// The data to sign
    #[arg(required = false, value_parser = parse_data)]
    // Note this weirdness is due to https://github.com/clap-rs/clap/discussions/4695
    // Need to specify the long name here due to invalid parsing in Clap which looks at the NAME rather than the TYPE which causes issues in how it handles Vec's.
    pub data: alloc::vec::Vec<u8>,
}

#[derive(Parser, Debug, Clone)]
/// Arguments for the Dkg command
pub struct RunDkgArgs {
    /// Path to config file
    #[arg(long, short, value_name = "FILE")]
    pub config: PathBuf,
    /// The reward cycle the signer is registered for and wants to peform DKG for
    #[arg(long, short)]
    pub reward_cycle: u64,
}

#[derive(Parser, Debug, Clone)]
/// Arguments for the Run command
pub struct RunSignerArgs {
    /// Path to config file
    #[arg(long, short, value_name = "FILE")]
    pub config: PathBuf,
}

#[derive(Parser, Debug, Clone)]
/// Arguments for the generate-files command
pub struct GenerateFilesArgs {
    /// The Stacks node to connect to
    #[arg(long)]
    pub host: SocketAddr,
    #[arg(
        long,
        required_unless_present = "private_keys",
        conflicts_with = "private_keys"
    )]
    /// The number of signers to generate
    pub num_signers: Option<u32>,
    #[clap(long, value_name = "FILE")]
    /// A path to a file containing a list of hexadecimal Stacks private keys of the signers
    pub private_keys: Option<PathBuf>,
    #[arg(long, value_parser = parse_network)]
    /// The network to use. One of "mainnet", "testnet", or "mocknet".
    pub network: Network,
    /// The directory to write the test data files to
    #[arg(long, default_value = ".")]
    pub dir: PathBuf,
    /// The number of milliseconds to wait when polling for events from the stacker-db instance.
    #[arg(long)]
    pub timeout: Option<u64>,
    #[arg(long)]
    /// The authorization password to use to connect to the validate block proposal node endpoint
    pub password: String,
}

#[derive(Clone, Debug)]
/// Wrapper around `Pox4SignatureTopic` to implement `ValueEnum`
pub struct StackingSignatureMethod(Pox4SignatureTopic);

impl StackingSignatureMethod {
    /// Get the inner `Pox4SignatureTopic`
    pub const fn topic(&self) -> &Pox4SignatureTopic {
        &self.0
    }
}

impl From<Pox4SignatureTopic> for StackingSignatureMethod {
    fn from(topic: Pox4SignatureTopic) -> Self {
        Self(topic)
    }
}

impl ValueEnum for StackingSignatureMethod {
    fn to_possible_value(&self) -> Option<clap::builder::PossibleValue> {
        Some(clap::builder::PossibleValue::new(self.0.get_name_str()))
    }

    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self(Pox4SignatureTopic::StackStx),
            Self(Pox4SignatureTopic::StackExtend),
            Self(Pox4SignatureTopic::AggregationCommit),
        ]
    }

    fn from_str(input: &str, _ignore_case: bool) -> Result<Self, String> {
        let topic = match input {
            "stack-stx" => Pox4SignatureTopic::StackStx,
            "stack-extend" => Pox4SignatureTopic::StackExtend,
            "aggregation-commit" => Pox4SignatureTopic::AggregationCommit,
            "agg-commit" => Pox4SignatureTopic::AggregationCommit,
            _ => return Err(format!("Invalid topic: {}", input)),
        };
        Ok(topic.into())
    }
}

#[derive(Parser, Debug, Clone)]
/// Arguments for the generate-stacking-signature command
pub struct GenerateStackingSignatureArgs {
    /// BTC address used to receive rewards
    #[arg(short, long, value_parser = parse_pox_addr)]
    pub pox_address: PoxAddress,
    /// The reward cycle during which this signature
    /// can be used
    #[arg(short, long)]
    pub reward_cycle: u64,
    /// Path to signer config file
    #[arg(long, short, value_name = "FILE")]
    pub config: PathBuf,
    /// Stacking method that can be used
    #[arg(long)]
    pub method: StackingSignatureMethod,
    /// Number of cycles used as a lock period.
    /// Use `1` for stack-aggregation-commit
    #[arg(long)]
    pub period: u64,
    /// The max amount of uSTX that can be used in this unique transaction
    #[arg(long)]
    pub max_amount: u128,
    /// A unique identifier to prevent re-using this authorization
    #[arg(long)]
    pub auth_id: u128,
    /// Output information in JSON format
    #[arg(long, action=ArgAction::SetTrue, required=false)]
    pub json: bool,
}

/// Parse the contract ID
fn parse_contract(contract: &str) -> Result<QualifiedContractIdentifier, String> {
    QualifiedContractIdentifier::parse(contract).map_err(|e| format!("Invalid contract: {}", e))
}

/// Parse a BTC address argument and return a `PoxAddress`.
/// This function behaves similarly to `PoxAddress::from_b58`, but also handles
/// addresses where the parsed AddressHashMode is None.
pub fn parse_pox_addr(pox_address_literal: &str) -> Result<PoxAddress, String> {
    let parsed_addr = PoxAddress::from_b58(pox_address_literal).map_or_else(
        || Err(format!("Invalid pox address: {pox_address_literal}")),
        Ok,
    );
    match parsed_addr {
        Ok(PoxAddress::Standard(addr, None)) => match addr.version {
            C32_ADDRESS_VERSION_MAINNET_MULTISIG | C32_ADDRESS_VERSION_TESTNET_MULTISIG => Ok(
                PoxAddress::Standard(addr, Some(AddressHashMode::SerializeP2SH)),
            ),
            C32_ADDRESS_VERSION_MAINNET_SINGLESIG | C32_ADDRESS_VERSION_TESTNET_SINGLESIG => Ok(
                PoxAddress::Standard(addr, Some(AddressHashMode::SerializeP2PKH)),
            ),
            _ => Err(format!("Invalid address version: {}", addr.version)),
        },
        _ => parsed_addr,
    }
}

/// Parse the hexadecimal Stacks private key
fn parse_private_key(private_key: &str) -> Result<StacksPrivateKey, String> {
    StacksPrivateKey::from_hex(private_key).map_err(|e| format!("Invalid private key: {}", e))
}

/// Parse the input data
fn parse_data(data: &str) -> Result<Vec<u8>, String> {
    let encoded_data = if data == "-" {
        // Parse the data from stdin
        let mut data = String::new();
        io::stdin().read_to_string(&mut data).unwrap();
        data
    } else {
        data.to_string()
    };
    let data =
        b58::from(&encoded_data).map_err(|e| format!("Failed to decode provided data: {}", e))?;
    Ok(data)
}

/// Parse the network. Must be one of "mainnet", "testnet", or "mocknet".
fn parse_network(network: &str) -> Result<Network, String> {
    Ok(match network.to_lowercase().as_str() {
        "mainnet" => Network::Mainnet,
        "testnet" => Network::Testnet,
        "mocknet" => Network::Mocknet,
        _ => {
            return Err(format!(
                "Invalid network: {}. Must be one of \"mainnet\", \"testnet\", or \"mocknet\".",
                network
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use blockstack_lib::chainstate::stacks::address::{PoxAddressType20, PoxAddressType32};
    use blockstack_lib::util_lib::signed_structured_data::pox4::make_pox_4_signer_key_message_hash;
    use clarity::consts::CHAIN_ID_TESTNET;
    use clarity::util::hash::Sha256Sum;

    use super::*;

    /// Helper just to ensure that a the pox address
    /// can be turned into a clarity tuple
    fn make_message_hash(pox_addr: &PoxAddress) -> Sha256Sum {
        make_pox_4_signer_key_message_hash(
            pox_addr,
            0,
            &Pox4SignatureTopic::StackStx,
            CHAIN_ID_TESTNET,
            0,
            0,
            0,
        )
    }

    fn clarity_tuple_version(pox_addr: &PoxAddress) -> u8 {
        pox_addr
            .as_clarity_tuple()
            .expect("Failed to generate clarity tuple for pox address")
            .get("version")
            .expect("Expected version in clarity tuple")
            .clone()
            .expect_buff(1)
            .expect("Expected version to be a u128")
            .get(0)
            .expect("Expected version to be a uint")
            .clone()
    }

    #[test]
    fn test_parse_pox_addr() {
        let tr = "bc1p8vg588hldsnv4a558apet4e9ff3pr4awhqj2hy8gy6x2yxzjpmqsvvpta4";
        let pox_addr = parse_pox_addr(tr).expect("Failed to parse segwit address");
        assert_eq!(tr, pox_addr.clone().to_b58());
        make_message_hash(&pox_addr);
        assert_eq!(
            clarity_tuple_version(&pox_addr),
            PoxAddressType32::P2TR.to_u8()
        );
        match pox_addr {
            PoxAddress::Addr32(_, addr_type, _) => {
                assert_eq!(addr_type, PoxAddressType32::P2TR);
            }
            _ => panic!("Invalid parsed address"),
        }

        let legacy = "1N8GMS991YDY1E696e9SB9EsYY5ckSU7hZ";
        let pox_addr = parse_pox_addr(legacy).expect("Failed to parse legacy address");
        assert_eq!(legacy, pox_addr.clone().to_b58());
        make_message_hash(&pox_addr);
        assert_eq!(
            clarity_tuple_version(&pox_addr),
            AddressHashMode::SerializeP2PKH as u8
        );
        match pox_addr {
            PoxAddress::Standard(stacks_addr, hash_mode) => {
                assert_eq!(stacks_addr.version, 22);
                assert_eq!(hash_mode, Some(AddressHashMode::SerializeP2PKH));
            }
            _ => panic!("Invalid parsed address"),
        }

        let p2sh = "33JNgVMNMC9Xm6mJG9oTVf5zWbmt5xi1Mv";
        let pox_addr = parse_pox_addr(p2sh).expect("Failed to parse legacy address");
        assert_eq!(p2sh, pox_addr.clone().to_b58());
        assert_eq!(
            clarity_tuple_version(&pox_addr),
            AddressHashMode::SerializeP2SH as u8
        );
        make_message_hash(&pox_addr);
        match pox_addr {
            PoxAddress::Standard(stacks_addr, hash_mode) => {
                assert_eq!(stacks_addr.version, 20);
                assert_eq!(hash_mode, Some(AddressHashMode::SerializeP2SH));
            }
            _ => panic!("Invalid parsed address"),
        }

        let testnet_p2pkh = "mnr5asd1MLSutHLL514WZXNpUNN3L98zBc";
        let pox_addr = parse_pox_addr(testnet_p2pkh).expect("Failed to parse testnet address");
        assert_eq!(
            clarity_tuple_version(&pox_addr),
            AddressHashMode::SerializeP2PKH as u8
        );
        assert_eq!(testnet_p2pkh, pox_addr.clone().to_b58());
        make_message_hash(&pox_addr);
        match pox_addr {
            PoxAddress::Standard(stacks_addr, hash_mode) => {
                assert_eq!(stacks_addr.version, C32_ADDRESS_VERSION_TESTNET_SINGLESIG);
                assert_eq!(hash_mode, Some(AddressHashMode::SerializeP2PKH));
            }
            _ => panic!("Invalid parsed address"),
        }

        let wsh = "bc1qvnpcphdctvmql5gdw6chtwvvsl6ra9gwa2nehc99np7f24juc4vqrx29cs";
        let pox_addr = parse_pox_addr(wsh).expect("Failed to parse segwit address");
        assert_eq!(
            clarity_tuple_version(&pox_addr),
            PoxAddressType32::P2WSH.to_u8()
        );
        assert_eq!(wsh, pox_addr.clone().to_b58());
        make_message_hash(&pox_addr);
        match pox_addr {
            PoxAddress::Addr32(_, addr_type, _) => {
                assert_eq!(addr_type, PoxAddressType32::P2WSH);
            }
            _ => panic!("Invalid parsed address"),
        }

        let wpkh = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";
        let pox_addr = parse_pox_addr(wpkh).expect("Failed to parse segwit address");
        assert_eq!(
            clarity_tuple_version(&pox_addr),
            PoxAddressType20::P2WPKH.to_u8()
        );
        assert_eq!(wpkh, pox_addr.clone().to_b58());
        make_message_hash(&pox_addr);
        match pox_addr {
            PoxAddress::Addr20(_, addr_type, _) => {
                assert_eq!(addr_type, PoxAddressType20::P2WPKH);
            }
            _ => panic!("Invalid parsed address"),
        }

        let testnet_tr = "tb1p46cgptxsfwkqpnnj552rkae3nf6l52wxn4snp4vm6mcrz2585hwq6cdwf2";
        let pox_addr = parse_pox_addr(testnet_tr).expect("Failed to parse testnet address");
        assert_eq!(testnet_tr, pox_addr.clone().to_b58());
        make_message_hash(&pox_addr);
        assert_eq!(
            clarity_tuple_version(&pox_addr),
            PoxAddressType32::P2TR.to_u8()
        );
        match pox_addr {
            PoxAddress::Addr32(_, addr_type, _) => {
                assert_eq!(addr_type, PoxAddressType32::P2TR);
            }
            _ => panic!("Invalid parsed address"),
        }

        let testnet_segwit = "tb1q38eleudmqyg4jrm39dnudj23pv6jcjrksa437s";
        let pox_addr = parse_pox_addr(testnet_segwit).expect("Failed to parse testnet address");
        assert_eq!(testnet_segwit, pox_addr.clone().to_b58());
        make_message_hash(&pox_addr);
        assert_eq!(
            clarity_tuple_version(&pox_addr),
            PoxAddressType20::P2WPKH.to_u8()
        );
        match pox_addr {
            PoxAddress::Addr20(_, addr_type, _) => {
                assert_eq!(addr_type, PoxAddressType20::P2WPKH);
            }
            _ => panic!("Invalid parsed address"),
        }
    }
}
