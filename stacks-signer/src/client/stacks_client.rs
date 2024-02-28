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
use std::net::SocketAddr;

use blockstack_lib::burnchains::Txid;
use blockstack_lib::chainstate::nakamoto::NakamotoBlock;
use blockstack_lib::chainstate::stacks::boot::{RewardSet, SIGNERS_NAME, SIGNERS_VOTING_NAME};
use blockstack_lib::chainstate::stacks::{
    StacksTransaction, StacksTransactionSigner, TransactionAnchorMode, TransactionAuth,
    TransactionContractCall, TransactionPayload, TransactionPostConditionMode,
    TransactionSpendingCondition, TransactionVersion,
};
use blockstack_lib::net::api::callreadonly::CallReadOnlyResponse;
use blockstack_lib::net::api::getaccount::AccountEntryResponse;
use blockstack_lib::net::api::getinfo::RPCPeerInfoData;
use blockstack_lib::net::api::getpoxinfo::RPCPoxInfoData;
use blockstack_lib::net::api::getstackers::GetStackersResponse;
use blockstack_lib::net::api::postblock_proposal::NakamotoBlockProposal;
use blockstack_lib::util_lib::boot::{boot_code_addr, boot_code_id};
use clarity::vm::types::{PrincipalData, QualifiedContractIdentifier};
use clarity::vm::{ClarityName, ContractName, Value as ClarityValue};
use hashbrown::{HashMap, HashSet};
use serde_json::json;
use slog::{slog_debug, slog_warn};
use stacks_common::codec::StacksMessageCodec;
use stacks_common::consts::{CHAIN_ID_MAINNET, CHAIN_ID_TESTNET};
use stacks_common::types::chainstate::{StacksAddress, StacksPrivateKey, StacksPublicKey};
use stacks_common::types::StacksEpochId;
use stacks_common::{debug, warn};
use wsts::curve::ecdsa;
use wsts::curve::point::{Compressed, Point};
use wsts::state_machine::PublicKeys;

use crate::client::{retry_with_exponential_backoff, ClientError};
use crate::config::{GlobalConfig, RegisteredSignersInfo};

/// The name of the function for casting a DKG result to signer vote contract
pub const VOTE_FUNCTION_NAME: &str = "vote-for-aggregate-public-key";

/// The Stacks signer client used to communicate with the stacks node
#[derive(Clone, Debug)]
pub struct StacksClient {
    /// The stacks address of the signer
    stacks_address: StacksAddress,
    /// The private key used in all stacks node communications
    stacks_private_key: StacksPrivateKey,
    /// The stacks node HTTP base endpoint
    http_origin: String,
    /// The types of transactions
    tx_version: TransactionVersion,
    /// The chain we are interacting with
    chain_id: u32,
    /// Whether we are mainnet or not
    mainnet: bool,
    /// The Client used to make HTTP connects
    stacks_node_client: reqwest::blocking::Client,
}

impl From<&GlobalConfig> for StacksClient {
    fn from(config: &GlobalConfig) -> Self {
        Self {
            stacks_private_key: config.stacks_private_key,
            stacks_address: config.stacks_address,
            http_origin: format!("http://{}", config.node_host),
            tx_version: config.network.to_transaction_version(),
            chain_id: config.network.to_chain_id(),
            stacks_node_client: reqwest::blocking::Client::new(),
            mainnet: config.network.is_mainnet(),
        }
    }
}

impl StacksClient {
    /// Create a new signer StacksClient with the provided private key, stacks node host endpoint, and version
    pub fn new(stacks_private_key: StacksPrivateKey, node_host: SocketAddr, mainnet: bool) -> Self {
        let pubkey = StacksPublicKey::from_private(&stacks_private_key);
        let tx_version = if mainnet {
            TransactionVersion::Mainnet
        } else {
            TransactionVersion::Testnet
        };
        let chain_id = if mainnet {
            CHAIN_ID_MAINNET
        } else {
            CHAIN_ID_TESTNET
        };
        let stacks_address = StacksAddress::p2pkh(mainnet, &pubkey);
        Self {
            stacks_private_key,
            stacks_address,
            http_origin: format!("http://{}", node_host),
            tx_version,
            chain_id,
            stacks_node_client: reqwest::blocking::Client::new(),
            mainnet,
        }
    }

    /// Get our signer address
    pub fn get_signer_address(&self) -> &StacksAddress {
        &self.stacks_address
    }

    /// Retrieve the signer slots stored within the stackerdb contract
    pub fn get_stackerdb_signer_slots(
        &self,
        stackerdb_contract: &QualifiedContractIdentifier,
        page: u32,
    ) -> Result<Vec<(StacksAddress, u128)>, ClientError> {
        let function_name_str = "stackerdb-get-signer-slots-page";
        let function_name = ClarityName::from(function_name_str);
        let function_args = &[ClarityValue::UInt(page.into())];
        let value = self.read_only_contract_call(
            &stackerdb_contract.issuer.clone().into(),
            &stackerdb_contract.name,
            &function_name,
            function_args,
        )?;
        self.parse_signer_slots(value)
    }

    /// Helper function  that attempts to deserialize a clarity hext string as a list of signer slots and their associated number of signer slots
    fn parse_signer_slots(
        &self,
        value: ClarityValue,
    ) -> Result<Vec<(StacksAddress, u128)>, ClientError> {
        debug!("Parsing signer slots...");
        let value = value.clone().expect_result_ok()?;
        let values = value.expect_list()?;
        let mut signer_slots = Vec::with_capacity(values.len());
        for value in values {
            let tuple_data = value.expect_tuple()?;
            let principal_data = tuple_data.get("signer")?.clone().expect_principal()?;
            let signer = if let PrincipalData::Standard(signer) = principal_data {
                signer.into()
            } else {
                panic!("BUG: Signers stackerdb contract is corrupted");
            };
            let num_slots = tuple_data.get("num-slots")?.clone().expect_u128()?;
            signer_slots.push((signer, num_slots));
        }
        Ok(signer_slots)
    }

    /// Get the vote for a given  round, reward cycle, and signer address
    pub fn get_vote_for_aggregate_public_key(
        &self,
        round: u64,
        reward_cycle: u64,
        signer: StacksAddress,
    ) -> Result<Option<Point>, ClientError> {
        debug!("Getting vote for aggregate public key...");
        let function_name = ClarityName::from("get-vote");
        let function_args = &[
            ClarityValue::UInt(reward_cycle as u128),
            ClarityValue::UInt(round as u128),
            ClarityValue::Principal(signer.into()),
        ];
        let value = self.read_only_contract_call(
            &boot_code_addr(self.mainnet),
            &ContractName::from(SIGNERS_VOTING_NAME),
            &function_name,
            function_args,
        )?;
        self.parse_aggregate_public_key(value)
    }

    /// Determine the stacks node current epoch
    pub fn get_node_epoch(&self) -> Result<StacksEpochId, ClientError> {
        let pox_info = self.get_pox_data()?;
        let burn_block_height = self.get_burn_block_height()?;

        let epoch_25 = pox_info
            .epochs
            .iter()
            .find(|epoch| epoch.epoch_id == StacksEpochId::Epoch25)
            .ok_or(ClientError::UnsupportedStacksFeature(
                "/v2/pox must report epochs".into(),
            ))?;

        let epoch_30 = pox_info
            .epochs
            .iter()
            .find(|epoch| epoch.epoch_id == StacksEpochId::Epoch30)
            .ok_or(ClientError::UnsupportedStacksFeature(
                "/v2/pox mut report epochs".into(),
            ))?;

        if burn_block_height < epoch_25.start_height {
            Ok(StacksEpochId::Epoch24)
        } else if burn_block_height < epoch_30.start_height {
            Ok(StacksEpochId::Epoch25)
        } else {
            Ok(StacksEpochId::Epoch30)
        }
    }

    /// Submit the block proposal to the stacks node. The block will be validated and returned via the HTTP endpoint for Block events.
    pub fn submit_block_for_validation(&self, block: NakamotoBlock) -> Result<(), ClientError> {
        let block_proposal = NakamotoBlockProposal {
            block,
            chain_id: self.chain_id,
        };
        let send_request = || {
            self.stacks_node_client
                .post(self.block_proposal_path())
                .header("Content-Type", "application/json")
                .json(&block_proposal)
                .send()
                .map_err(backoff::Error::transient)
        };

        let response = retry_with_exponential_backoff(send_request)?;
        if !response.status().is_success() {
            return Err(ClientError::RequestFailure(response.status()));
        }
        Ok(())
    }

    /// Retrieve the approved DKG aggregate public key for the given reward cycle
    pub fn get_approved_aggregate_key(
        &self,
        reward_cycle: u64,
    ) -> Result<Option<Point>, ClientError> {
        let function_name = ClarityName::from("get-approved-aggregate-key");
        let pox_contract_id = boot_code_id(SIGNERS_VOTING_NAME, self.mainnet);
        let function_args = &[ClarityValue::UInt(reward_cycle as u128)];
        let value = self.read_only_contract_call(
            &pox_contract_id.issuer.into(),
            &pox_contract_id.name,
            &function_name,
            function_args,
        )?;
        self.parse_aggregate_public_key(value)
    }

    /// Retrieve the current account nonce for the provided address
    pub fn get_account_nonce(&self, address: &StacksAddress) -> Result<u64, ClientError> {
        let account_entry = self.get_account_entry(address)?;
        Ok(account_entry.nonce)
    }

    /// Get the current peer info data from the stacks node
    pub fn get_peer_info(&self) -> Result<RPCPeerInfoData, ClientError> {
        debug!("Getting stacks node info...");
        let send_request = || {
            self.stacks_node_client
                .get(self.core_info_path())
                .send()
                .map_err(backoff::Error::transient)
        };
        let response = retry_with_exponential_backoff(send_request)?;
        if !response.status().is_success() {
            return Err(ClientError::RequestFailure(response.status()));
        }
        let peer_info_data = response.json::<RPCPeerInfoData>()?;
        Ok(peer_info_data)
    }

    /// Retrieve the last DKG vote round number for the current reward cycle
    pub fn get_last_round(&self, reward_cycle: u64) -> Result<Option<u64>, ClientError> {
        debug!("Getting the last DKG vote round of reward cycle {reward_cycle}...");
        let contract_addr = boot_code_addr(self.mainnet);
        let contract_name = ContractName::from(SIGNERS_VOTING_NAME);
        let function_name = ClarityName::from("get-last-round");
        let function_args = &[ClarityValue::UInt(reward_cycle as u128)];
        let opt_value = self
            .read_only_contract_call(
                &contract_addr,
                &contract_name,
                &function_name,
                function_args,
            )?
            .expect_optional()?;
        let round = if let Some(value) = opt_value {
            Some(u64::try_from(value.expect_u128()?).map_err(|e| {
                ClientError::MalformedContractData(format!(
                    "Failed to convert vote round to u64: {e}"
                ))
            })?)
        } else {
            None
        };
        Ok(round)
    }

    /// Get the reward set from the stacks node for the given reward cycle
    pub fn get_reward_set(&self, reward_cycle: u64) -> Result<RewardSet, ClientError> {
        debug!("Getting reward set for reward cycle {reward_cycle}...");
        let send_request = || {
            self.stacks_node_client
                .get(self.reward_set_path(reward_cycle))
                .send()
                .map_err(backoff::Error::transient)
        };
        let response = retry_with_exponential_backoff(send_request)?;
        if !response.status().is_success() {
            return Err(ClientError::RequestFailure(response.status()));
        }
        let stackers_response = response.json::<GetStackersResponse>()?;
        Ok(stackers_response.stacker_set)
    }

    /// Get the registered signers for a specific reward cycle
    /// Returns None if no signers are registered or its not Nakamoto cycle
    pub fn get_registered_signers_info(
        &self,
        reward_cycle: u64,
    ) -> Result<Option<RegisteredSignersInfo>, ClientError> {
        debug!("Getting registered signers for reward cycle {reward_cycle}...");
        let reward_set = self.get_reward_set(reward_cycle)?;
        let Some(reward_set_signers) = reward_set.signers else {
            warn!("No reward set signers found for reward cycle {reward_cycle}.");
            return Ok(None);
        };
        if reward_set_signers.is_empty() {
            warn!("No registered signers found for reward cycle {reward_cycle}.");
            return Ok(None);
        }
        // signer uses a Vec<u32> for its key_ids, but coordinator uses a HashSet for each signer since it needs to do lots of lookups
        let mut weight_end = 1;
        let mut coordinator_key_ids = HashMap::with_capacity(4000);
        let mut signer_key_ids = HashMap::with_capacity(reward_set_signers.len());
        let mut signer_ids = HashMap::with_capacity(reward_set_signers.len());
        let mut public_keys = PublicKeys {
            signers: HashMap::with_capacity(reward_set_signers.len()),
            key_ids: HashMap::with_capacity(4000),
        };
        let mut signer_public_keys = HashMap::with_capacity(reward_set_signers.len());
        for (i, entry) in reward_set_signers.iter().enumerate() {
            let signer_id = u32::try_from(i).expect("FATAL: number of signers exceeds u32::MAX");
            let ecdsa_public_key = ecdsa::PublicKey::try_from(entry.signing_key.as_slice()).map_err(|e| {
                ClientError::CorruptedRewardSet(format!(
                        "Reward cycle {reward_cycle} failed to convert signing key to ecdsa::PublicKey: {e}"
                    ))
                })?;
            let signer_public_key = Point::try_from(&Compressed::from(ecdsa_public_key.to_bytes()))
                .map_err(|e| {
                    ClientError::CorruptedRewardSet(format!(
                        "Reward cycle {reward_cycle} failed to convert signing key to Point: {e}"
                    ))
                })?;
            let stacks_public_key = StacksPublicKey::from_slice(entry.signing_key.as_slice()).map_err(|e| {
                    ClientError::CorruptedRewardSet(format!(
                        "Reward cycle {reward_cycle} failed to convert signing key to StacksPublicKey: {e}"
                    ))
                })?;

            let stacks_address = StacksAddress::p2pkh(self.mainnet, &stacks_public_key);

            signer_ids.insert(stacks_address, signer_id);
            signer_public_keys.insert(signer_id, signer_public_key);
            let weight_start = weight_end;
            weight_end = weight_start + entry.weight;
            for key_id in weight_start..weight_end {
                public_keys.key_ids.insert(key_id, ecdsa_public_key);
                public_keys.signers.insert(signer_id, ecdsa_public_key);
                coordinator_key_ids
                    .entry(signer_id)
                    .or_insert(HashSet::with_capacity(entry.weight as usize))
                    .insert(key_id);
                signer_key_ids
                    .entry(signer_id)
                    .or_insert(Vec::with_capacity(entry.weight as usize))
                    .push(key_id);
            }
        }

        let signer_set =
            u32::try_from(reward_cycle % 2).expect("FATAL: reward_cycle % 2 exceeds u32::MAX");
        let signer_stackerdb_contract_id = boot_code_id(SIGNERS_NAME, self.mainnet);
        // Get the signer writers from the stacker-db to find the signer slot id
        let signer_slots_weights = self
            .get_stackerdb_signer_slots(&signer_stackerdb_contract_id, signer_set)
            .unwrap();
        let mut signer_slot_ids = HashMap::with_capacity(signer_slots_weights.len());
        for (index, (address, _)) in signer_slots_weights.into_iter().enumerate() {
            signer_slot_ids.insert(
                address,
                u32::try_from(index).expect("FATAL: number of signers exceeds u32::MAX"),
            );
        }

        for address in signer_ids.keys() {
            if !signer_slot_ids.contains_key(address) {
                debug!("Signer {address} does not have a slot id in the stackerdb");
                return Ok(None);
            }
        }

        Ok(Some(RegisteredSignersInfo {
            public_keys,
            signer_key_ids,
            signer_ids,
            signer_slot_ids,
            signer_public_keys,
            coordinator_key_ids,
        }))
    }

    /// Retreive the current pox data from the stacks node
    pub fn get_pox_data(&self) -> Result<RPCPoxInfoData, ClientError> {
        debug!("Getting pox data...");
        let send_request = || {
            self.stacks_node_client
                .get(self.pox_path())
                .send()
                .map_err(backoff::Error::transient)
        };
        let response = retry_with_exponential_backoff(send_request)?;
        if !response.status().is_success() {
            return Err(ClientError::RequestFailure(response.status()));
        }
        let pox_info_data = response.json::<RPCPoxInfoData>()?;
        Ok(pox_info_data)
    }

    /// Helper function to retrieve the burn tip height from the stacks node
    fn get_burn_block_height(&self) -> Result<u64, ClientError> {
        let peer_info = self.get_peer_info()?;
        Ok(peer_info.burn_block_height)
    }

    /// Get the current reward cycle from the stacks node
    pub fn get_current_reward_cycle(&self) -> Result<u64, ClientError> {
        let pox_data = self.get_pox_data()?;
        let blocks_mined = pox_data
            .current_burnchain_block_height
            .saturating_sub(pox_data.first_burnchain_block_height);
        let reward_cycle_length = pox_data
            .reward_phase_block_length
            .saturating_add(pox_data.prepare_phase_block_length);
        Ok(blocks_mined / reward_cycle_length)
    }

    /// Helper function to retrieve the account info from the stacks node for a specific address
    fn get_account_entry(
        &self,
        address: &StacksAddress,
    ) -> Result<AccountEntryResponse, ClientError> {
        debug!("Getting account info...");
        let send_request = || {
            self.stacks_node_client
                .get(self.accounts_path(address))
                .send()
                .map_err(backoff::Error::transient)
        };
        let response = retry_with_exponential_backoff(send_request)?;
        if !response.status().is_success() {
            return Err(ClientError::RequestFailure(response.status()));
        }
        let account_entry = response.json::<AccountEntryResponse>()?;
        Ok(account_entry)
    }

    /// Helper function that attempts to deserialize a clarity hex string as the aggregate public key
    fn parse_aggregate_public_key(
        &self,
        value: ClarityValue,
    ) -> Result<Option<Point>, ClientError> {
        debug!("Parsing aggregate public key...");
        let opt = value.clone().expect_optional()?;
        let Some(inner_data) = opt else {
            return Ok(None);
        };
        let data = inner_data.expect_buff(33)?;
        // It is possible that the point was invalid though when voted upon and this cannot be prevented by pox 4 definitions...
        // Pass up this error if the conversions fail.
        let compressed_data = Compressed::try_from(data.as_slice()).map_err(|e| {
            ClientError::MalformedClarityValue(format!(
                "Failed to convert aggregate public key to compressed data: {e}"
            ))
        })?;
        let point = Point::try_from(&compressed_data).map_err(|e| {
            ClientError::MalformedClarityValue(format!(
                "Failed to convert aggregate public key to a point: {e}"
            ))
        })?;
        Ok(Some(point))
    }

    /// Helper function to create a stacks transaction for a modifying contract call
    pub fn build_vote_for_aggregate_public_key(
        &self,
        signer_index: u32,
        round: u64,
        point: Point,
        reward_cycle: u64,
        tx_fee: Option<u64>,
        nonce: u64,
    ) -> Result<StacksTransaction, ClientError> {
        debug!("Building {VOTE_FUNCTION_NAME} transaction...");
        let contract_address = boot_code_addr(self.mainnet);
        let contract_name = ContractName::from(SIGNERS_VOTING_NAME);
        let function_name = ClarityName::from(VOTE_FUNCTION_NAME);
        let function_args = vec![
            ClarityValue::UInt(signer_index as u128),
            ClarityValue::buff_from(point.compress().data.to_vec())?,
            ClarityValue::UInt(round as u128),
            ClarityValue::UInt(reward_cycle as u128),
        ];
        let tx_fee = tx_fee.unwrap_or(0);

        Self::build_signed_contract_call_transaction(
            &contract_address,
            contract_name,
            function_name,
            &function_args,
            &self.stacks_private_key,
            self.tx_version,
            self.chain_id,
            nonce,
            tx_fee,
        )
    }

    /// Helper function to submit a transaction to the Stacks mempool
    pub fn submit_transaction(&self, tx: &StacksTransaction) -> Result<Txid, ClientError> {
        let txid = tx.txid();
        let tx = tx.serialize_to_vec();
        let send_request = || {
            self.stacks_node_client
                .post(self.transaction_path())
                .header("Content-Type", "application/octet-stream")
                .body(tx.clone())
                .send()
                .map_err(|e| {
                    debug!("Failed to submit transaction to the Stacks node: {e:?}");
                    backoff::Error::transient(e)
                })
        };
        let response = retry_with_exponential_backoff(send_request)?;
        if !response.status().is_success() {
            return Err(ClientError::RequestFailure(response.status()));
        }
        Ok(txid)
    }

    /// Makes a read only contract call to a stacks contract
    pub fn read_only_contract_call(
        &self,
        contract_addr: &StacksAddress,
        contract_name: &ContractName,
        function_name: &ClarityName,
        function_args: &[ClarityValue],
    ) -> Result<ClarityValue, ClientError> {
        debug!("Calling read-only function {function_name} with args {function_args:?}...");
        let args = function_args
            .iter()
            .filter_map(|arg| arg.serialize_to_hex().ok())
            .collect::<Vec<String>>();
        if args.len() != function_args.len() {
            return Err(ClientError::ReadOnlyFailure(
                "Failed to serialize Clarity function arguments".into(),
            ));
        }

        let body =
            json!({"sender": self.stacks_address.to_string(), "arguments": args}).to_string();
        let path = self.read_only_path(contract_addr, contract_name, function_name);
        let response = self
            .stacks_node_client
            .post(path.clone())
            .header("Content-Type", "application/json")
            .body(body.clone())
            .send()?;
        if !response.status().is_success() {
            return Err(ClientError::RequestFailure(response.status()));
        }
        let call_read_only_response = response.json::<CallReadOnlyResponse>()?;
        if !call_read_only_response.okay {
            return Err(ClientError::ReadOnlyFailure(format!(
                "{function_name}: {}",
                call_read_only_response
                    .cause
                    .unwrap_or("unknown".to_string())
            )));
        }
        let hex = call_read_only_response.result.unwrap_or_default();
        let value = ClarityValue::try_deserialize_hex_untyped(&hex)?;
        Ok(value)
    }

    fn pox_path(&self) -> String {
        format!("{}/v2/pox", self.http_origin)
    }

    fn transaction_path(&self) -> String {
        format!("{}/v2/transactions", self.http_origin)
    }

    fn read_only_path(
        &self,
        contract_addr: &StacksAddress,
        contract_name: &ContractName,
        function_name: &ClarityName,
    ) -> String {
        format!(
            "{}/v2/contracts/call-read/{contract_addr}/{contract_name}/{function_name}",
            self.http_origin
        )
    }

    fn block_proposal_path(&self) -> String {
        format!("{}/v2/block_proposal", self.http_origin)
    }

    fn core_info_path(&self) -> String {
        format!("{}/v2/info", self.http_origin)
    }

    fn accounts_path(&self, stacks_address: &StacksAddress) -> String {
        format!("{}/v2/accounts/{stacks_address}?proof=0", self.http_origin)
    }

    fn reward_set_path(&self, reward_cycle: u64) -> String {
        format!("{}/v2/stacker_set/{reward_cycle}", self.http_origin)
    }

    /// Helper function to create a stacks transaction for a modifying contract call
    #[allow(clippy::too_many_arguments)]
    pub fn build_signed_contract_call_transaction(
        contract_addr: &StacksAddress,
        contract_name: ContractName,
        function_name: ClarityName,
        function_args: &[ClarityValue],
        stacks_private_key: &StacksPrivateKey,
        tx_version: TransactionVersion,
        chain_id: u32,
        nonce: u64,
        tx_fee: u64,
    ) -> Result<StacksTransaction, ClientError> {
        let tx_payload = TransactionPayload::ContractCall(TransactionContractCall {
            address: *contract_addr,
            contract_name,
            function_name,
            function_args: function_args.to_vec(),
        });
        let public_key = StacksPublicKey::from_private(stacks_private_key);
        let tx_auth = TransactionAuth::Standard(
            TransactionSpendingCondition::new_singlesig_p2pkh(public_key).ok_or(
                ClientError::TransactionGenerationFailure(format!(
                    "Failed to create spending condition from public key: {}",
                    public_key.to_hex()
                )),
            )?,
        );

        let mut unsigned_tx = StacksTransaction::new(tx_version, tx_auth, tx_payload);

        unsigned_tx.set_tx_fee(tx_fee);
        unsigned_tx.set_origin_nonce(nonce);

        unsigned_tx.anchor_mode = TransactionAnchorMode::Any;
        unsigned_tx.post_condition_mode = TransactionPostConditionMode::Allow;
        unsigned_tx.chain_id = chain_id;

        let mut tx_signer = StacksTransactionSigner::new(&unsigned_tx);
        tx_signer
            .sign_origin(stacks_private_key)
            .map_err(|e| ClientError::TransactionGenerationFailure(e.to_string()))?;

        tx_signer
            .get_tx()
            .ok_or(ClientError::TransactionGenerationFailure(
                "Failed to generate transaction from a transaction signer".to_string(),
            ))
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufWriter, Write};
    use std::thread::spawn;

    use blockstack_lib::chainstate::nakamoto::NakamotoBlockHeader;
    use blockstack_lib::chainstate::stacks::address::PoxAddress;
    use blockstack_lib::chainstate::stacks::boot::{NakamotoSignerEntry, PoxStartCycleInfo};
    use blockstack_lib::chainstate::stacks::ThresholdSignature;
    use rand::thread_rng;
    use rand_core::RngCore;
    use serial_test::serial;
    use stacks_common::bitvec::BitVec;
    use stacks_common::consts::{CHAIN_ID_TESTNET, SIGNER_SLOTS_PER_USER};
    use stacks_common::types::chainstate::{ConsensusHash, StacksBlockId, TrieHash};
    use stacks_common::util::hash::Sha512Trunc256Sum;
    use stacks_common::util::secp256k1::MessageSignature;
    use wsts::curve::scalar::Scalar;

    use super::*;
    use crate::client::tests::{
        build_account_nonce_response, build_get_approved_aggregate_key_response,
        build_get_last_round_response, build_get_peer_info_response, build_get_pox_data_response,
        build_read_only_response, write_response, MockServerClient,
    };

    #[test]
    fn read_only_contract_call_200_success() {
        let mock = MockServerClient::new();
        let value = ClarityValue::UInt(10_u128);
        let response = build_read_only_response(&value);
        let h = spawn(move || {
            mock.client.read_only_contract_call(
                &mock.client.stacks_address,
                &ContractName::from("contract-name"),
                &ClarityName::from("function-name"),
                &[],
            )
        });
        write_response(mock.server, response.as_bytes());
        let result = h.join().unwrap().unwrap();
        assert_eq!(result, value);
    }

    #[test]
    fn read_only_contract_call_with_function_args_200_success() {
        let mock = MockServerClient::new();
        let value = ClarityValue::UInt(10_u128);
        let response = build_read_only_response(&value);
        let h = spawn(move || {
            mock.client.read_only_contract_call(
                &mock.client.stacks_address,
                &ContractName::from("contract-name"),
                &ClarityName::from("function-name"),
                &[ClarityValue::UInt(10_u128)],
            )
        });
        write_response(mock.server, response.as_bytes());
        let result = h.join().unwrap().unwrap();
        assert_eq!(result, value);
    }

    #[test]
    fn read_only_contract_call_200_failure() {
        let mock = MockServerClient::new();
        let h = spawn(move || {
            mock.client.read_only_contract_call(
                &mock.client.stacks_address,
                &ContractName::from("contract-name"),
                &ClarityName::from("function-name"),
                &[],
            )
        });
        write_response(
            mock.server,
            b"HTTP/1.1 200 OK\n\n{\"okay\":false,\"cause\":\"Some reason\"}",
        );
        let result = h.join().unwrap();
        assert!(matches!(result, Err(ClientError::ReadOnlyFailure(_))));
    }

    #[test]
    fn read_only_contract_call_400_failure() {
        let mock = MockServerClient::new();
        // Simulate a 400 Bad Request response
        let h = spawn(move || {
            mock.client.read_only_contract_call(
                &mock.client.stacks_address,
                &ContractName::from("contract-name"),
                &ClarityName::from("function-name"),
                &[],
            )
        });
        write_response(mock.server, b"HTTP/1.1 400 Bad Request\n\n");
        let result = h.join().unwrap();
        assert!(matches!(
            result,
            Err(ClientError::RequestFailure(
                reqwest::StatusCode::BAD_REQUEST
            ))
        ));
    }

    #[test]
    fn read_only_contract_call_404_failure() {
        let mock = MockServerClient::new();
        // Simulate a 400 Bad Request response
        let h = spawn(move || {
            mock.client.read_only_contract_call(
                &mock.client.stacks_address,
                &ContractName::from("contract-name"),
                &ClarityName::from("function-name"),
                &[],
            )
        });
        write_response(mock.server, b"HTTP/1.1 404 Not Found\n\n");
        let result = h.join().unwrap();
        assert!(matches!(
            result,
            Err(ClientError::RequestFailure(reqwest::StatusCode::NOT_FOUND))
        ));
    }

    #[test]
    fn valid_reward_cycle_should_succeed() {
        let mock = MockServerClient::new();
        let (pox_data_response, pox_data) = build_get_pox_data_response(None, None, None, None);
        let h = spawn(move || mock.client.get_current_reward_cycle());
        write_response(mock.server, pox_data_response.as_bytes());
        let current_cycle_id = h.join().unwrap().unwrap();
        let blocks_mined = pox_data
            .current_burnchain_block_height
            .saturating_sub(pox_data.first_burnchain_block_height);
        let reward_cycle_length = pox_data
            .reward_phase_block_length
            .saturating_add(pox_data.prepare_phase_block_length);
        let id = blocks_mined / reward_cycle_length;
        assert_eq!(current_cycle_id, id);
    }

    #[test]
    fn invalid_reward_cycle_should_fail() {
        let mock = MockServerClient::new();
        let h = spawn(move || mock.client.get_current_reward_cycle());
        write_response(
            mock.server,
            b"HTTP/1.1 200 Ok\n\n{\"current_cycle\":{\"id\":\"fake id\", \"is_pox_active\":false}}",
        );
        let res = h.join().unwrap();
        assert!(matches!(res, Err(ClientError::ReqwestError(_))));
    }

    #[test]
    fn get_aggregate_public_key_should_succeed() {
        let orig_point = Point::from(Scalar::random(&mut rand::thread_rng()));
        let response = build_get_approved_aggregate_key_response(Some(orig_point));
        let mock = MockServerClient::new();
        let h = spawn(move || mock.client.get_approved_aggregate_key(0));
        write_response(mock.server, response.as_bytes());
        let res = h.join().unwrap().unwrap();
        assert_eq!(res, Some(orig_point));

        let response = build_get_approved_aggregate_key_response(None);
        let mock = MockServerClient::new();
        let h = spawn(move || mock.client.get_approved_aggregate_key(0));
        write_response(mock.server, response.as_bytes());
        let res = h.join().unwrap().unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn parse_valid_aggregate_public_key_should_succeed() {
        let mock = MockServerClient::new();
        let orig_point = Point::from(Scalar::random(&mut rand::thread_rng()));
        let clarity_value = ClarityValue::some(
            ClarityValue::buff_from(orig_point.compress().as_bytes().to_vec())
                .expect("BUG: Failed to create clarity value from point"),
        )
        .expect("BUG: Failed to create clarity value from point");
        let result = mock
            .client
            .parse_aggregate_public_key(clarity_value)
            .unwrap();
        assert_eq!(result, Some(orig_point));

        let value = ClarityValue::none();
        let result = mock.client.parse_aggregate_public_key(value).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_invalid_aggregate_public_key_should_fail() {
        let mock = MockServerClient::new();
        let value = ClarityValue::UInt(10_u128);
        let result = mock.client.parse_aggregate_public_key(value);
        assert!(result.is_err())
    }

    #[ignore]
    #[test]
    fn transaction_contract_call_should_send_bytes_to_node() {
        let mock = MockServerClient::new();
        let private_key = StacksPrivateKey::new();
        let tx = StacksClient::build_signed_contract_call_transaction(
            &mock.client.stacks_address,
            ContractName::from("contract-name"),
            ClarityName::from("function-name"),
            &[],
            &private_key,
            TransactionVersion::Testnet,
            CHAIN_ID_TESTNET,
            0,
            10_000,
        )
        .unwrap();

        let mut tx_bytes = [0u8; 1024];
        {
            let mut tx_bytes_writer = BufWriter::new(&mut tx_bytes[..]);
            tx.consensus_serialize(&mut tx_bytes_writer).unwrap();
            tx_bytes_writer.flush().unwrap();
        }

        let bytes_len = tx_bytes
            .iter()
            .enumerate()
            .rev()
            .find(|(_, &x)| x != 0)
            .unwrap()
            .0
            + 1;

        let tx_clone = tx.clone();
        let h = spawn(move || mock.client.submit_transaction(&tx_clone));

        let request_bytes = write_response(
            mock.server,
            format!("HTTP/1.1 200 OK\n\n{}", tx.txid()).as_bytes(),
        );
        let returned_txid = h.join().unwrap().unwrap();

        assert_eq!(returned_txid, tx.txid());
        assert!(
            request_bytes
                .windows(bytes_len)
                .any(|window| window == &tx_bytes[..bytes_len]),
            "Request bytes did not contain the transaction bytes"
        );
    }

    #[ignore]
    #[test]
    #[serial]
    fn build_vote_for_aggregate_public_key_should_succeed() {
        let mock = MockServerClient::new();
        let point = Point::from(Scalar::random(&mut rand::thread_rng()));
        let nonce = thread_rng().next_u64();
        let signer_index = thread_rng().next_u32();
        let round = thread_rng().next_u64();
        let reward_cycle = thread_rng().next_u64();

        let h = spawn(move || {
            mock.client.build_vote_for_aggregate_public_key(
                signer_index,
                round,
                point,
                reward_cycle,
                None,
                nonce,
            )
        });
        assert!(h.join().unwrap().is_ok());
    }

    #[ignore]
    #[test]
    #[serial]
    fn broadcast_vote_for_aggregate_public_key_should_succeed() {
        let mock = MockServerClient::new();
        let point = Point::from(Scalar::random(&mut rand::thread_rng()));
        let nonce = thread_rng().next_u64();
        let signer_index = thread_rng().next_u32();
        let round = thread_rng().next_u64();
        let reward_cycle = thread_rng().next_u64();

        let h = spawn(move || {
            let tx = mock
                .client
                .clone()
                .build_vote_for_aggregate_public_key(
                    signer_index,
                    round,
                    point,
                    reward_cycle,
                    None,
                    nonce,
                )
                .unwrap();
            mock.client.submit_transaction(&tx)
        });
        let mock = MockServerClient::from_config(mock.config);
        write_response(
            mock.server,
            b"HTTP/1.1 200 OK\n\n4e99f99bc4a05437abb8c7d0c306618f45b203196498e2ebe287f10497124958",
        );
        assert!(h.join().unwrap().is_ok());
    }

    #[test]
    fn core_info_call_for_burn_block_height_should_succeed() {
        let mock = MockServerClient::new();
        let h = spawn(move || mock.client.get_burn_block_height());
        let (response, peer_info) = build_get_peer_info_response(None, None);
        write_response(mock.server, response.as_bytes());
        let burn_block_height = h.join().unwrap().expect("Failed to deserialize response");
        assert_eq!(burn_block_height, peer_info.burn_block_height);
    }

    #[test]
    fn core_info_call_for_burn_block_height_should_fail() {
        let mock = MockServerClient::new();
        let h = spawn(move || mock.client.get_burn_block_height());
        write_response(
            mock.server,
            b"HTTP/1.1 200 OK\n\n4e99f99bc4a05437abb8c7d0c306618f45b203196498e2ebe287f10497124958",
        );
        assert!(h.join().unwrap().is_err());
    }

    #[test]
    fn get_account_nonce_should_succeed() {
        let mock = MockServerClient::new();
        let address = mock.client.stacks_address;
        let h = spawn(move || mock.client.get_account_nonce(&address));
        let nonce = thread_rng().next_u64();
        write_response(mock.server, build_account_nonce_response(nonce).as_bytes());
        let returned_nonce = h.join().unwrap().expect("Failed to deserialize response");
        assert_eq!(returned_nonce, nonce);
    }

    #[test]
    fn get_account_nonce_should_fail() {
        let mock = MockServerClient::new();
        let address = mock.client.stacks_address;
        let h = spawn(move || mock.client.get_account_nonce(&address));
        write_response(
            mock.server,
            b"HTTP/1.1 200 OK\n\n{\"nonce\":\"invalid nonce\",\"balance\":\"0x00000000000000000000000000000000\",\"locked\":\"0x00000000000000000000000000000000\",\"unlock_height\":0}"
        );
        assert!(h.join().unwrap().is_err());
    }

    #[test]
    fn parse_valid_signer_slots_should_succeed() {
        let mock = MockServerClient::new();
        let clarity_value_hex =
            "0x070b000000050c00000002096e756d2d736c6f7473010000000000000000000000000000000c067369676e6572051a8195196a9a7cf9c37cb13e1ed69a7bc047a84e050c00000002096e756d2d736c6f7473010000000000000000000000000000000c067369676e6572051a6505471146dcf722f0580911183f28bef30a8a890c00000002096e756d2d736c6f7473010000000000000000000000000000000c067369676e6572051a1d7f8e3936e5da5f32982cc47f31d7df9fb1b38a0c00000002096e756d2d736c6f7473010000000000000000000000000000000c067369676e6572051a126d1a814313c952e34c7840acec9211e1727fb80c00000002096e756d2d736c6f7473010000000000000000000000000000000c067369676e6572051a7374ea6bb39f2e8d3d334d62b9f302a977de339a";
        let value = ClarityValue::try_deserialize_hex_untyped(clarity_value_hex).unwrap();
        let signer_slots = mock.client.parse_signer_slots(value).unwrap();
        assert_eq!(signer_slots.len(), 5);
        signer_slots
            .into_iter()
            .for_each(|(_address, slots)| assert!(slots == SIGNER_SLOTS_PER_USER as u128));
    }

    #[test]
    fn get_node_epoch_should_succeed() {
        let mock = MockServerClient::new();
        // The burn block height is one BEHIND the activation height of 2.5, therefore is 2.4
        let burn_block_height: u64 = 100;
        let pox_response = build_get_pox_data_response(
            None,
            None,
            Some(burn_block_height.saturating_add(1)),
            None,
        )
        .0;
        let peer_response = build_get_peer_info_response(Some(burn_block_height), None).0;
        let h = spawn(move || mock.client.get_node_epoch());
        write_response(mock.server, pox_response.as_bytes());
        let mock = MockServerClient::from_config(mock.config);
        write_response(mock.server, peer_response.as_bytes());
        let epoch = h.join().unwrap().expect("Failed to deserialize response");
        assert_eq!(epoch, StacksEpochId::Epoch24);

        // The burn block height is the same as the activation height of 2.5, therefore is 2.5
        let pox_response = build_get_pox_data_response(None, None, Some(burn_block_height), None).0;
        let peer_response = build_get_peer_info_response(Some(burn_block_height), None).0;
        let mock = MockServerClient::from_config(mock.config);
        let h = spawn(move || mock.client.get_node_epoch());
        write_response(mock.server, pox_response.as_bytes());
        let mock = MockServerClient::from_config(mock.config);
        write_response(mock.server, peer_response.as_bytes());
        let epoch = h.join().unwrap().expect("Failed to deserialize response");
        assert_eq!(epoch, StacksEpochId::Epoch25);

        // The burn block height is the AFTER as the activation height of 2.5 but BEFORE the activation height of 3.0, therefore is 2.5
        let pox_response = build_get_pox_data_response(
            None,
            None,
            Some(burn_block_height.saturating_sub(1)),
            Some(burn_block_height.saturating_add(1)),
        )
        .0;
        let peer_response = build_get_peer_info_response(Some(burn_block_height), None).0;
        let mock = MockServerClient::from_config(mock.config);
        let h = spawn(move || mock.client.get_node_epoch());
        write_response(mock.server, pox_response.as_bytes());
        let mock = MockServerClient::from_config(mock.config);
        write_response(mock.server, peer_response.as_bytes());
        let epoch = h.join().unwrap().expect("Failed to deserialize response");
        assert_eq!(epoch, StacksEpochId::Epoch25);

        // The burn block height is the AFTER as the activation height of 2.5 and the SAME as the activation height of 3.0, therefore is 3.0
        let pox_response = build_get_pox_data_response(
            None,
            None,
            Some(burn_block_height.saturating_sub(1)),
            Some(burn_block_height),
        )
        .0;
        let peer_response = build_get_peer_info_response(Some(burn_block_height), None).0;
        let mock = MockServerClient::from_config(mock.config);
        let h = spawn(move || mock.client.get_node_epoch());
        write_response(mock.server, pox_response.as_bytes());
        let mock = MockServerClient::from_config(mock.config);
        write_response(mock.server, peer_response.as_bytes());
        let epoch = h.join().unwrap().expect("Failed to deserialize response");
        assert_eq!(epoch, StacksEpochId::Epoch30);

        // The burn block height is the AFTER as the activation height of 2.5 and AFTER the activation height of 3.0, therefore is 3.0
        let pox_response = build_get_pox_data_response(
            None,
            None,
            Some(burn_block_height.saturating_sub(1)),
            Some(burn_block_height),
        )
        .0;
        let peer_response =
            build_get_peer_info_response(Some(burn_block_height.saturating_add(1)), None).0;
        let mock = MockServerClient::from_config(mock.config);
        let h = spawn(move || mock.client.get_node_epoch());
        write_response(mock.server, pox_response.as_bytes());
        let mock = MockServerClient::from_config(mock.config);
        write_response(mock.server, peer_response.as_bytes());
        let epoch = h.join().unwrap().expect("Failed to deserialize response");
        assert_eq!(epoch, StacksEpochId::Epoch30);
    }

    #[test]
    fn get_node_epoch_should_fail() {
        let mock = MockServerClient::new();
        let h = spawn(move || mock.client.get_node_epoch());
        write_response(
            mock.server,
            b"HTTP/1.1 200 OK\n\n4e99f99bc4a05437abb8c7d0c306618f45b203196498e2ebe287f10497124958",
        );
        assert!(h.join().unwrap().is_err());
    }

    #[test]
    fn submit_block_for_validation_should_succeed() {
        let mock = MockServerClient::new();
        let header = NakamotoBlockHeader {
            version: 1,
            chain_length: 2,
            burn_spent: 3,
            consensus_hash: ConsensusHash([0x04; 20]),
            parent_block_id: StacksBlockId([0x05; 32]),
            tx_merkle_root: Sha512Trunc256Sum([0x06; 32]),
            state_index_root: TrieHash([0x07; 32]),
            miner_signature: MessageSignature::empty(),
            signer_signature: ThresholdSignature::empty(),
            signer_bitvec: BitVec::zeros(1).unwrap(),
        };
        let block = NakamotoBlock {
            header,
            txs: vec![],
        };
        let h = spawn(move || mock.client.submit_block_for_validation(block));
        write_response(mock.server, b"HTTP/1.1 200 OK\n\n");
        assert!(h.join().unwrap().is_ok());
    }

    #[test]
    fn submit_block_for_validation_should_fail() {
        let mock = MockServerClient::new();
        let header = NakamotoBlockHeader {
            version: 1,
            chain_length: 2,
            burn_spent: 3,
            consensus_hash: ConsensusHash([0x04; 20]),
            parent_block_id: StacksBlockId([0x05; 32]),
            tx_merkle_root: Sha512Trunc256Sum([0x06; 32]),
            state_index_root: TrieHash([0x07; 32]),
            miner_signature: MessageSignature::empty(),
            signer_signature: ThresholdSignature::empty(),
            signer_bitvec: BitVec::zeros(1).unwrap(),
        };
        let block = NakamotoBlock {
            header,
            txs: vec![],
        };
        let h = spawn(move || mock.client.submit_block_for_validation(block));
        write_response(mock.server, b"HTTP/1.1 404 Not Found\n\n");
        assert!(h.join().unwrap().is_err());
    }

    #[test]
    fn get_peer_info_should_succeed() {
        let mock = MockServerClient::new();
        let (response, peer_info) = build_get_peer_info_response(None, None);
        let h = spawn(move || mock.client.get_peer_info());
        write_response(mock.server, response.as_bytes());
        assert_eq!(h.join().unwrap().unwrap(), peer_info);
    }

    #[test]
    fn get_last_round_should_succeed() {
        let mock = MockServerClient::new();
        let round = rand::thread_rng().next_u64();
        let response = build_get_last_round_response(round);
        let h = spawn(move || mock.client.get_last_round(0));

        write_response(mock.server, response.as_bytes());
        assert_eq!(h.join().unwrap().unwrap().unwrap(), round);
    }

    #[test]
    fn get_reward_set_should_succeed() {
        let mock = MockServerClient::new();
        let point = Point::from(Scalar::random(&mut rand::thread_rng())).compress();
        let mut bytes = [0u8; 33];
        bytes.copy_from_slice(point.as_bytes());
        let stacker_set = RewardSet {
            rewarded_addresses: vec![PoxAddress::standard_burn_address(false)],
            start_cycle_state: PoxStartCycleInfo {
                missed_reward_slots: vec![],
            },
            signers: Some(vec![NakamotoSignerEntry {
                signing_key: bytes,
                stacked_amt: rand::thread_rng().next_u64() as u128,
                weight: 1,
            }]),
        };
        let stackers_response = GetStackersResponse {
            stacker_set: stacker_set.clone(),
        };

        let stackers_response_json = serde_json::to_string(&stackers_response)
            .expect("Failed to serialize get stacker response");
        let response = format!("HTTP/1.1 200 OK\n\n{stackers_response_json}");
        let h = spawn(move || mock.client.get_reward_set(0));
        write_response(mock.server, response.as_bytes());
        assert_eq!(h.join().unwrap().unwrap(), stacker_set);
    }

    #[test]
    fn get_vote_for_aggregate_public_key_should_succeed() {
        let mock = MockServerClient::new();
        let point = Point::from(Scalar::random(&mut rand::thread_rng()));
        let stacks_address = mock.client.stacks_address;
        let key_response = build_get_approved_aggregate_key_response(Some(point));
        let h = spawn(move || {
            mock.client
                .get_vote_for_aggregate_public_key(0, 0, stacks_address)
        });
        write_response(mock.server, key_response.as_bytes());
        assert_eq!(h.join().unwrap().unwrap(), Some(point));

        let mock = MockServerClient::new();
        let stacks_address = mock.client.stacks_address;
        let key_response = build_get_approved_aggregate_key_response(None);
        let h = spawn(move || {
            mock.client
                .get_vote_for_aggregate_public_key(0, 0, stacks_address)
        });
        write_response(mock.server, key_response.as_bytes());
        assert_eq!(h.join().unwrap().unwrap(), None);
    }
}
