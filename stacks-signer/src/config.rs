// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2023 Stacks Open Internet Foundation
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

use std::convert::TryFrom;
use std::fs;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use blockstack_lib::chainstate::stacks::TransactionVersion;
use clarity::vm::types::QualifiedContractIdentifier;
use hashbrown::HashMap;
use p256k1::ecdsa;
use p256k1::scalar::Scalar;
use serde::Deserialize;
use stacks_common::address::{
    AddressHashMode, C32_ADDRESS_VERSION_MAINNET_SINGLESIG, C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
};
use stacks_common::consts::{CHAIN_ID_MAINNET, CHAIN_ID_TESTNET};
use stacks_common::types::chainstate::{StacksAddress, StacksPrivateKey, StacksPublicKey};
use wsts::state_machine::PublicKeys;

/// List of key_ids for each signer_id
pub type SignerKeyIds = HashMap<u32, Vec<u32>>;

const EVENT_TIMEOUT_MS: u64 = 5000;

#[derive(thiserror::Error, Debug)]
/// An error occurred parsing the provided configuration
pub enum ConfigError {
    /// Error occurred reading config file
    #[error("{0}")]
    InvalidConfig(String),
    /// An error occurred parsing the TOML data
    #[error("{0}")]
    ParseError(String),
    /// A field was malformed
    #[error("identifier={0}, value={1}")]
    BadField(String, String),
    /// An unsupported address version
    #[error("Failed to convert private key to address: unsupported address version.")]
    UnsupportedAddressVersion,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "lowercase")]
/// The Stacks network to use.
pub enum Network {
    /// The mainnet network
    Mainnet,
    /// The testnet network
    Testnet,
    /// The mocknet network
    Mocknet,
}

impl Network {
    /// Converts a Network enum variant to a corresponding chain id
    pub fn to_chain_id(&self) -> u32 {
        match self {
            Self::Mainnet => CHAIN_ID_MAINNET,
            Self::Testnet | Self::Mocknet => CHAIN_ID_TESTNET,
        }
    }

    /// Convert a Network enum variant to a corresponding address version
    pub fn to_address_version(&self) -> u8 {
        match self {
            Self::Mainnet => C32_ADDRESS_VERSION_MAINNET_SINGLESIG,
            Self::Testnet | Self::Mocknet => C32_ADDRESS_VERSION_TESTNET_SINGLESIG,
        }
    }

    /// Convert a Network enum variant to a Transaction Version
    pub fn to_transaction_version(&self) -> TransactionVersion {
        match self {
            Self::Mainnet => TransactionVersion::Mainnet,
            Self::Testnet | Self::Mocknet => TransactionVersion::Testnet,
        }
    }
}

/// The parsed configuration for the signer
pub struct Config {
    /// endpoint to the stacks node
    pub node_host: SocketAddr,
    /// endpoint to the stackerdb receiver
    pub endpoint: SocketAddr,
    /// smart contract that controls the target stackerdb
    pub stackerdb_contract_id: QualifiedContractIdentifier,
    /// smart contract that controls the target stackerdb
    pub pox_contract_id: Option<QualifiedContractIdentifier>,
    /// The Scalar representation of the private key for signer communication
    pub message_private_key: Scalar,
    /// The signer's Stacks private key
    pub stacks_private_key: StacksPrivateKey,
    /// The signer's Stacks address
    pub stacks_address: StacksAddress,
    /// The network to use. One of "mainnet" or "testnet".
    pub network: Network,
    /// The signer ID and key ids mapped to a public key
    pub signer_ids_public_keys: PublicKeys,
    /// The signer IDs mapped to their Key IDs
    pub signer_key_ids: SignerKeyIds,
    /// This signer's ID
    pub signer_id: u32,
    /// The time to wait for a response from the stacker-db instance
    pub event_timeout: Duration,
}

/// Internal struct for loading up the config file signer data
#[derive(Clone, Deserialize, Default, Debug)]
struct RawSigners {
    pub public_key: String,
    pub key_ids: Vec<u32>,
}

/// Internal struct for loading up the config file
#[derive(Deserialize, Debug)]
struct RawConfigFile {
    /// endpoint to stacks node
    pub node_host: String,
    /// endpoint to stackerdb receiver
    pub endpoint: String,
    // FIXME: these contract's should go away in non testing scenarios. Make them both optionals.
    /// Stacker db contract identifier
    pub stackerdb_contract_id: String,
    /// pox contract identifier
    pub pox_contract_id: Option<String>,
    /// the 32 byte ECDSA private key used to sign blocks, chunks, and transactions
    pub message_private_key: String,
    /// The hex representation of the signer's Stacks private key used for communicating
    /// with the Stacks Node, including writing to the Stacker DB instance.
    pub stacks_private_key: String,
    /// The network to use. One of "mainnet" or "testnet".
    pub network: Network,
    // TODO: Optionally retrieve the signers from the pox contract
    // See: https://github.com/stacks-network/stacks-blockchain/issues/3912
    /// The signers, IDs, and their private keys
    pub signers: Vec<RawSigners>,
    /// The signer ID
    pub signer_id: u32,
    /// The time to wait (in millisecs) for a response from the stacker-db instance
    pub event_timeout: Option<u64>,
}

impl RawConfigFile {
    /// load the config from a string
    pub fn load_from_str(data: &str) -> Result<Self, ConfigError> {
        let config: RawConfigFile =
            toml::from_str(data).map_err(|e| ConfigError::ParseError(format!("{:?}", &e)))?;
        Ok(config)
    }
    /// load the config from a file and parse it
    #[allow(dead_code)]
    pub fn load_from_file(path: &str) -> Result<Self, ConfigError> {
        Self::try_from(&PathBuf::from(path))
    }
}

impl TryFrom<&PathBuf> for RawConfigFile {
    type Error = ConfigError;

    fn try_from(path: &PathBuf) -> Result<Self, Self::Error> {
        RawConfigFile::load_from_str(&fs::read_to_string(path).map_err(|e| {
            ConfigError::InvalidConfig(format!("failed to read config file: {:?}", &e))
        })?)
    }
}

impl TryFrom<RawConfigFile> for Config {
    type Error = ConfigError;

    /// Attempt to decode the raw config file's primitive types into our types.
    /// NOTE: network access is required for this to work
    fn try_from(raw_data: RawConfigFile) -> Result<Self, Self::Error> {
        let node_host = raw_data
            .node_host
            .clone()
            .to_socket_addrs()
            .map_err(|_| {
                ConfigError::BadField("node_host".to_string(), raw_data.node_host.clone())
            })?
            .next()
            .ok_or(ConfigError::BadField(
                "node_host".to_string(),
                raw_data.node_host.clone(),
            ))?;

        let endpoint = raw_data
            .endpoint
            .clone()
            .to_socket_addrs()
            .map_err(|_| ConfigError::BadField("endpoint".to_string(), raw_data.endpoint.clone()))?
            .next()
            .ok_or(ConfigError::BadField(
                "endpoint".to_string(),
                raw_data.endpoint.clone(),
            ))?;

        let stackerdb_contract_id =
            QualifiedContractIdentifier::parse(&raw_data.stackerdb_contract_id).map_err(|_| {
                ConfigError::BadField(
                    "stackerdb_contract_id".to_string(),
                    raw_data.stackerdb_contract_id,
                )
            })?;

        let pox_contract_id = if let Some(id) = raw_data.pox_contract_id.as_ref() {
            Some(QualifiedContractIdentifier::parse(id).map_err(|_| {
                ConfigError::BadField(
                    "pox_contract_id".to_string(),
                    raw_data.pox_contract_id.unwrap_or("".to_string()),
                )
            })?)
        } else {
            None
        };

        let message_private_key =
            Scalar::try_from(raw_data.message_private_key.as_str()).map_err(|_| {
                ConfigError::BadField(
                    "message_private_key".to_string(),
                    raw_data.message_private_key.clone(),
                )
            })?;

        let stacks_private_key =
            StacksPrivateKey::from_hex(&raw_data.stacks_private_key).map_err(|_| {
                ConfigError::BadField(
                    "stacks_private_key".to_string(),
                    raw_data.stacks_private_key.clone(),
                )
            })?;
        let stacks_public_key = StacksPublicKey::from_private(&stacks_private_key);
        let stacks_address = StacksAddress::from_public_keys(
            raw_data.network.to_address_version(),
            &AddressHashMode::SerializeP2PKH,
            1,
            &vec![stacks_public_key],
        )
        .ok_or(ConfigError::UnsupportedAddressVersion)?;
        let mut public_keys = PublicKeys::default();
        let mut signer_key_ids = SignerKeyIds::default();
        for (i, s) in raw_data.signers.iter().enumerate() {
            let signer_public_key =
                ecdsa::PublicKey::try_from(s.public_key.as_str()).map_err(|_| {
                    ConfigError::BadField("signers.public_key".to_string(), s.public_key.clone())
                })?;
            for key_id in &s.key_ids {
                //We do not allow a key id of 0.
                if *key_id == 0 {
                    return Err(ConfigError::BadField(
                        "signers.key_ids".to_string(),
                        key_id.to_string(),
                    ));
                }
                public_keys.key_ids.insert(*key_id, signer_public_key);
            }
            //We start our signer and key IDs from 1 hence the + 1;
            let signer_key = u32::try_from(i).unwrap();
            public_keys.signers.insert(signer_key, signer_public_key);
            signer_key_ids.insert(signer_key, s.key_ids.clone());
        }
        let event_timeout =
            Duration::from_millis(raw_data.event_timeout.unwrap_or(EVENT_TIMEOUT_MS));
        Ok(Self {
            node_host,
            endpoint,
            stackerdb_contract_id,
            pox_contract_id,
            message_private_key,
            stacks_private_key,
            stacks_address,
            network: raw_data.network,
            signer_ids_public_keys: public_keys,
            signer_id: raw_data.signer_id,
            signer_key_ids,
            event_timeout,
        })
    }
}

impl TryFrom<&PathBuf> for Config {
    type Error = ConfigError;
    fn try_from(path: &PathBuf) -> Result<Self, ConfigError> {
        let config_file = RawConfigFile::try_from(path)?;
        Self::try_from(config_file)
    }
}

impl Config {
    /// load the config from a string and parse it
    #[allow(dead_code)]
    pub fn load_from_str(data: &str) -> Result<Self, ConfigError> {
        RawConfigFile::load_from_str(data)?.try_into()
    }

    /// load the config from a file and parse it
    #[allow(dead_code)]
    pub fn load_from_file(path: &str) -> Result<Self, ConfigError> {
        Self::try_from(&PathBuf::from(path))
    }
}
