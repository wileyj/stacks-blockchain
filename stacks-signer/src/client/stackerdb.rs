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
//
use std::net::SocketAddr;

use blockstack_lib::chainstate::nakamoto::signer_set::NakamotoSigners;
use blockstack_lib::chainstate::stacks::StacksTransaction;
use blockstack_lib::util_lib::boot::boot_code_addr;
use clarity::vm::types::QualifiedContractIdentifier;
use clarity::vm::ContractName;
use hashbrown::HashMap;
use libsigner::{SignerMessage, SignerSession, StackerDBSession, TRANSACTIONS_MSG_ID};
use libstackerdb::{StackerDBChunkAckData, StackerDBChunkData};
use slog::{slog_debug, slog_warn};
use stacks_common::codec::{read_next, StacksMessageCodec};
use stacks_common::consts::SIGNER_SLOTS_PER_USER;
use stacks_common::types::chainstate::StacksPrivateKey;
use stacks_common::{debug, warn};

use super::ClientError;
use crate::client::retry_with_exponential_backoff;
use crate::config::SignerConfig;

/// The StackerDB client for communicating with the .signers contract
pub struct StackerDB {
    /// The stacker-db sessions for each signer set and message type.
    /// Maps message ID to the DB session.
    signers_message_stackerdb_sessions: HashMap<u32, StackerDBSession>,
    /// The private key used in all stacks node communications
    stacks_private_key: StacksPrivateKey,
    /// A map of a message ID to last chunk version for each session
    slot_versions: HashMap<u32, HashMap<u32, u32>>,
    /// The signer slot ID -- the index into the signer list for this signer daemon's signing key.
    signer_slot_id: u32,
    /// The reward cycle of the connecting signer
    reward_cycle: u64,
    /// The stacker-db transaction msg session for the NEXT reward cycle
    next_transaction_session: StackerDBSession,
}

impl From<&SignerConfig> for StackerDB {
    fn from(config: &SignerConfig) -> Self {
        StackerDB::new(
            config.node_host,
            config.stacks_private_key,
            config.mainnet,
            config.reward_cycle,
            config.signer_slot_id,
        )
    }
}
impl StackerDB {
    /// Create a new StackerDB client
    pub fn new(
        host: SocketAddr,
        stacks_private_key: StacksPrivateKey,
        is_mainnet: bool,
        reward_cycle: u64,
        signer_slot_id: u32,
    ) -> Self {
        let mut signers_message_stackerdb_sessions = HashMap::new();
        let stackerdb_issuer = boot_code_addr(is_mainnet);
        for msg_id in 0..SIGNER_SLOTS_PER_USER {
            signers_message_stackerdb_sessions.insert(
                msg_id,
                StackerDBSession::new(
                    host,
                    QualifiedContractIdentifier::new(
                        stackerdb_issuer.into(),
                        ContractName::from(
                            NakamotoSigners::make_signers_db_name(reward_cycle, msg_id).as_str(),
                        ),
                    ),
                ),
            );
        }
        let next_transaction_session = StackerDBSession::new(
            host,
            QualifiedContractIdentifier::new(
                stackerdb_issuer.into(),
                ContractName::from(
                    NakamotoSigners::make_signers_db_name(
                        reward_cycle.wrapping_add(1),
                        TRANSACTIONS_MSG_ID,
                    )
                    .as_str(),
                ),
            ),
        );

        Self {
            signers_message_stackerdb_sessions,
            stacks_private_key,
            slot_versions: HashMap::new(),
            signer_slot_id,
            reward_cycle,
            next_transaction_session,
        }
    }

    /// Sends messages to the .signers stacker-db with an exponential backoff retry
    pub fn send_message_with_retry(
        &mut self,
        message: SignerMessage,
    ) -> Result<StackerDBChunkAckData, ClientError> {
        let message_bytes = message.serialize_to_vec();
        let msg_id = message.msg_id();
        let slot_id = self.signer_slot_id;
        loop {
            let slot_version = if let Some(versions) = self.slot_versions.get_mut(&msg_id) {
                if let Some(version) = versions.get(&slot_id) {
                    *version
                } else {
                    versions.insert(slot_id, 0);
                    1
                }
            } else {
                let mut versions = HashMap::new();
                versions.insert(slot_id, 0);
                self.slot_versions.insert(msg_id, versions);
                1
            };

            let mut chunk = StackerDBChunkData::new(slot_id, slot_version, message_bytes.clone());
            chunk.sign(&self.stacks_private_key)?;

            let Some(session) = self.signers_message_stackerdb_sessions.get_mut(&msg_id) else {
                panic!("FATAL: would loop forever trying to send a message with ID {}, for which we don't have a session", msg_id);
            };

            debug!(
                "Sending a chunk to stackerdb slot ID {slot_id} with version {slot_version} to contract {:?}!\n{chunk:?}",
                &session.stackerdb_contract_id
            );

            let send_request = || session.put_chunk(&chunk).map_err(backoff::Error::transient);
            let chunk_ack: StackerDBChunkAckData = retry_with_exponential_backoff(send_request)?;

            if let Some(versions) = self.slot_versions.get_mut(&msg_id) {
                // NOTE: per the above, this is always executed
                versions.insert(slot_id, slot_version.saturating_add(1));
            } else {
                return Err(ClientError::NotConnected);
            }

            if chunk_ack.accepted {
                debug!("Chunk accepted by stackerdb: {chunk_ack:?}");
                return Ok(chunk_ack);
            } else {
                warn!("Chunk rejected by stackerdb: {chunk_ack:?}");
            }
            if let Some(reason) = chunk_ack.reason {
                // TODO: fix this jankiness. Update stackerdb to use an error code mapping instead of just a string
                // See: https://github.com/stacks-network/stacks-blockchain/issues/3917
                if reason.contains("Data for this slot and version already exist") {
                    warn!("Failed to send message to stackerdb due to wrong version number {}. Incrementing and retrying...", slot_version);
                    if let Some(versions) = self.slot_versions.get_mut(&msg_id) {
                        // NOTE: per the above, this is always executed
                        versions.insert(slot_id, slot_version.saturating_add(1));
                    } else {
                        return Err(ClientError::NotConnected);
                    }
                } else {
                    warn!("Failed to send message to stackerdb: {}", reason);
                    return Err(ClientError::PutChunkRejected(reason));
                }
            }
        }
    }

    /// Get the transactions from stackerdb for the signers
    fn get_transactions(
        transactions_session: &mut StackerDBSession,
        signer_ids: &[u32],
    ) -> Result<Vec<StacksTransaction>, ClientError> {
        let send_request = || {
            transactions_session
                .get_latest_chunks(signer_ids)
                .map_err(backoff::Error::transient)
        };
        let chunk_ack = retry_with_exponential_backoff(send_request)?;
        let mut transactions = Vec::new();
        for (i, chunk) in chunk_ack.iter().enumerate() {
            let signer_id = *signer_ids
                .get(i)
                .expect("BUG: retrieved an unequal amount of chunks to requested chunks");
            let Some(data) = chunk else {
                continue;
            };
            let Ok(message) = read_next::<SignerMessage, _>(&mut &data[..]) else {
                if !data.is_empty() {
                    warn!("Failed to deserialize chunk data into a SignerMessage");
                    debug!(
                        "signer #{signer_id}: Failed chunk ({}): {data:?}",
                        &data.len(),
                    );
                }
                continue;
            };

            let SignerMessage::Transactions(chunk_transactions) = message else {
                warn!("Signer wrote an unexpected type to the transactions slot");
                continue;
            };
            debug!(
                "Retrieved {} transactions from signer ID {}.",
                chunk_transactions.len(),
                signer_id
            );
            transactions.extend(chunk_transactions);
        }
        Ok(transactions)
    }

    /// Get the latest signer transactions from signer ids for the current reward cycle
    pub fn get_current_transactions_with_retry(
        &mut self,
        signer_id: u32,
    ) -> Result<Vec<StacksTransaction>, ClientError> {
        debug!("Signer #{signer_id}: Getting latest transactions from stacker db",);
        let Some(transactions_session) = self
            .signers_message_stackerdb_sessions
            .get_mut(&TRANSACTIONS_MSG_ID)
        else {
            return Err(ClientError::NotConnected);
        };
        Self::get_transactions(transactions_session, &[signer_id])
    }

    /// Get the latest signer transactions from signer ids for the next reward cycle
    pub fn get_next_transactions_with_retry(
        &mut self,
        signer_ids: &[u32],
    ) -> Result<Vec<StacksTransaction>, ClientError> {
        debug!("Getting latest chunks from stackerdb for the following signers: {signer_ids:?}",);
        Self::get_transactions(&mut self.next_transaction_session, signer_ids)
    }

    /// Retrieve the signer set this stackerdb client is attached to
    pub fn get_signer_set(&self) -> u32 {
        u32::try_from(self.reward_cycle % 2).expect("FATAL: reward cycle % 2 exceeds u32::MAX")
    }

    /// Retrieve the signer slot ID
    pub fn get_signer_slot_id(&mut self) -> u32 {
        self.signer_slot_id
    }
}

#[cfg(test)]
mod tests {
    use std::thread::spawn;
    use std::time::Duration;

    use blockstack_lib::chainstate::stacks::{
        TransactionAnchorMode, TransactionAuth, TransactionPayload, TransactionPostConditionMode,
        TransactionSmartContract, TransactionVersion,
    };
    use blockstack_lib::util_lib::strings::StacksString;
    use serial_test::serial;

    use super::*;
    use crate::client::tests::{generate_signer_config, mock_server_from_config, write_response};
    use crate::config::GlobalConfig;

    #[test]
    #[serial]
    fn get_signer_transactions_with_retry_should_succeed() {
        let config = GlobalConfig::load_from_file("./src/tests/conf/signer-0.toml").unwrap();
        let signer_config = generate_signer_config(&config, 5, 20);
        let mut stackerdb = StackerDB::from(&signer_config);
        let sk = StacksPrivateKey::new();
        let tx = StacksTransaction {
            version: TransactionVersion::Testnet,
            chain_id: 0,
            auth: TransactionAuth::from_p2pkh(&sk).unwrap(),
            anchor_mode: TransactionAnchorMode::Any,
            post_condition_mode: TransactionPostConditionMode::Allow,
            post_conditions: vec![],
            payload: TransactionPayload::SmartContract(
                TransactionSmartContract {
                    name: "test-contract".into(),
                    code_body: StacksString::from_str("(/ 1 0)").unwrap(),
                },
                None,
            ),
        };

        let signer_message = SignerMessage::Transactions(vec![tx.clone()]);
        let message = signer_message.serialize_to_vec();

        let signer_ids = vec![0, 1];
        let h = spawn(move || stackerdb.get_next_transactions_with_retry(&signer_ids));
        let mut response_bytes = b"HTTP/1.1 200 OK\n\n".to_vec();
        response_bytes.extend(message);
        let mock_server = mock_server_from_config(&config);
        write_response(mock_server, response_bytes.as_slice());

        let signer_message = SignerMessage::Transactions(vec![]);
        let message = signer_message.serialize_to_vec();
        let mut response_bytes = b"HTTP/1.1 200 OK\n\n".to_vec();
        response_bytes.extend(message);
        let mock_server = mock_server_from_config(&config);
        write_response(mock_server, response_bytes.as_slice());

        let transactions = h.join().unwrap().unwrap();
        assert_eq!(transactions, vec![tx]);
    }

    #[test]
    #[serial]
    fn send_signer_message_with_retry_should_succeed() {
        let config = GlobalConfig::load_from_file("./src/tests/conf/signer-0.toml").unwrap();
        let signer_config = generate_signer_config(&config, 5, 20);
        let mut stackerdb = StackerDB::from(&signer_config);

        let sk = StacksPrivateKey::new();
        let tx = StacksTransaction {
            version: TransactionVersion::Testnet,
            chain_id: 0,
            auth: TransactionAuth::from_p2pkh(&sk).unwrap(),
            anchor_mode: TransactionAnchorMode::Any,
            post_condition_mode: TransactionPostConditionMode::Allow,
            post_conditions: vec![],
            payload: TransactionPayload::SmartContract(
                TransactionSmartContract {
                    name: "test-contract".into(),
                    code_body: StacksString::from_str("(/ 1 0)").unwrap(),
                },
                None,
            ),
        };

        let signer_message = SignerMessage::Transactions(vec![tx]);
        let ack = StackerDBChunkAckData {
            accepted: true,
            reason: None,
            metadata: None,
        };
        let mock_server = mock_server_from_config(&config);
        let h = spawn(move || stackerdb.send_message_with_retry(signer_message));
        let mut response_bytes = b"HTTP/1.1 200 OK\n\n".to_vec();
        let payload = serde_json::to_string(&ack).expect("Failed to serialize ack");
        response_bytes.extend(payload.as_bytes());
        std::thread::sleep(Duration::from_millis(500));
        write_response(mock_server, response_bytes.as_slice());
        assert_eq!(ack, h.join().unwrap().unwrap());
    }
}
