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

use std::collections::{BTreeMap, HashMap};

use stacks_common::bitvec::BitVec;
use stacks_common::util::get_epoch_time_secs;

use crate::burnchains::PoxConstants;
use crate::chainstate::burn::db::sortdb::SortitionDB;
use crate::chainstate::burn::{BlockSnapshot, ConsensusHash};
use crate::chainstate::nakamoto::NakamotoChainState;
use crate::chainstate::stacks::db::StacksChainState;
use crate::net::db::PeerDB;
use crate::net::neighbors::comms::PeerNetworkComms;
use crate::net::p2p::PeerNetwork;
use crate::net::{
    Error as NetError, GetNakamotoInvData, NakamotoInvData, NeighborAddress, NeighborComms,
    NeighborKey, StacksMessage, StacksMessageType,
};
use crate::util_lib::db::Error as DBError;

/// Cached data for a sortition in the sortition DB.
/// Caching this allows us to avoid calls to `SortitionDB::get_block_snapshot_consensus()`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InvSortitionInfo {
    parent_consensus_hash: ConsensusHash,
    block_height: u64,
}

impl InvSortitionInfo {
    /// Load up cacheable sortition state for a given consensus hash
    pub fn load(
        sortdb: &SortitionDB,
        consensus_hash: &ConsensusHash,
    ) -> Result<InvSortitionInfo, NetError> {
        let sn = SortitionDB::get_block_snapshot_consensus(sortdb.conn(), consensus_hash)?
            .ok_or(DBError::NotFoundError)?;

        let parent_sn = SortitionDB::get_block_snapshot(sortdb.conn(), &sn.parent_sortition_id)?
            .ok_or(DBError::NotFoundError)?;

        Ok(Self {
            parent_consensus_hash: parent_sn.consensus_hash,
            block_height: sn.block_height,
        })
    }
}

/// Cached data for a TenureChange transaction caused by a BlockFound event.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InvTenureInfo {
    /// This tenure's start-block consensus hash
    tenure_id_consensus_hash: ConsensusHash,
    /// This tenure's parent's start-block consensus hash
    parent_tenure_id_consensus_hash: ConsensusHash,
}

impl InvTenureInfo {
    /// Load up cacheable tenure state for a given tenure-ID consensus hash.
    /// This only returns Ok(Some(..)) if there was a tenure-change tx for this consensus hash.
    pub fn load(
        chainstate: &StacksChainState,
        consensus_hash: &ConsensusHash,
    ) -> Result<Option<InvTenureInfo>, NetError> {
        Ok(
            NakamotoChainState::get_highest_nakamoto_tenure_change_by_tenure_id(
                chainstate.db(),
                consensus_hash,
            )?
            .map(|tenure| Self {
                tenure_id_consensus_hash: tenure.tenure_id_consensus_hash,
                parent_tenure_id_consensus_hash: tenure.prev_tenure_id_consensus_hash,
            }),
        )
    }
}

/// This struct represents cached inventory data loaded from Nakamoto headers.
/// It is of the utmost importance that inventory message generation is _fast_, and incurs as
/// little I/O overhead as possible, given how essential these messages are to nodes trying to keep
/// in sync.  By caching (immutable) tenure data in this struct, we can enusre that this happens
/// all the time except for during node bootup.
pub struct InvGenerator {
    processed_tenures: HashMap<ConsensusHash, Option<InvTenureInfo>>,
    sortitions: HashMap<ConsensusHash, InvSortitionInfo>,
}

impl InvGenerator {
    pub fn new() -> Self {
        Self {
            processed_tenures: HashMap::new(),
            sortitions: HashMap::new(),
        }
    }

    /// Get a processed tenure. If it's not cached, then load it.
    /// Returns Some(..) if there existed a tenure-change tx for this given consensus hash
    fn get_processed_tenure(
        &mut self,
        chainstate: &StacksChainState,
        tenure_id_consensus_hash: &ConsensusHash,
    ) -> Result<Option<InvTenureInfo>, NetError> {
        if let Some(info_opt) = self.processed_tenures.get(&tenure_id_consensus_hash) {
            return Ok((*info_opt).clone());
        };
        // not cached so go load it
        let loaded_info_opt = InvTenureInfo::load(chainstate, &tenure_id_consensus_hash)?;
        self.processed_tenures
            .insert(tenure_id_consensus_hash.clone(), loaded_info_opt.clone());
        Ok(loaded_info_opt)
    }

    /// Generate an block inventory bit vector for a reward cycle.
    /// The bit vector is "big-endian" -- the first bit is the oldest sortition, and the last bit is
    /// the newest sortition.  It is structured as follows:
    /// * Bit 0 is the sortition at the start of the given reward cycle
    /// * Bit i is 1 if there was a tenure-start for the ith sortition in the reward cycle, and 0
    /// if not.
    ///
    /// Populate the cached data lazily.
    ///
    /// * `tip` is the canonical sortition tip
    /// * `chainstate` is a handle to the chainstate DB
    /// * `reward_cycle` is the reward cycle for which to generate the inventory
    ///
    /// The resulting bitvector will be truncated if `reward_cycle` is the current reward cycle.
    pub fn make_tenure_bitvector(
        &mut self,
        tip: &BlockSnapshot,
        sortdb: &SortitionDB,
        chainstate: &StacksChainState,
        reward_cycle: u64,
    ) -> Result<Vec<bool>, NetError> {
        let ih = sortdb.index_handle(&tip.sortition_id);
        let reward_cycle_end_height = sortdb
            .pox_constants
            .reward_cycle_to_block_height(sortdb.first_block_height, reward_cycle + 1)
            - 2;
        let reward_cycle_end_tip = if tip.block_height <= reward_cycle_end_height {
            tip.clone()
        } else {
            ih.get_block_snapshot_by_height(reward_cycle_end_height)?
                .ok_or(NetError::NotFoundError)?
        };

        let mut tenure_status = vec![];
        let mut cur_height = reward_cycle_end_tip.block_height;
        let mut cur_consensus_hash = reward_cycle_end_tip.consensus_hash;

        let mut cur_tenure_opt = self.get_processed_tenure(chainstate, &cur_consensus_hash)?;

        // loop variables and invariants:
        //
        // * `cur_height` is a "cursor" that gets used to populate the bitmap. It corresponds
        // to a burnchain block height (since inventory bitvectors correspond to sortitions).
        // It gets decremented once per loop pass.  The loop terminates once the reward cycle
        // for `cur_height` is less than the given `reward_cycle`.
        //
        // * `cur_consensus_hash` refers to the consensus hash of the sortition at `cur_height`. It
        // is updated once per loop pass.
        //
        // * `tenure_status` is the bit vector itself.  On each pass of this loop, `true` or
        // `false` is pushed to it.  When the loop exits, `tenure_status` will have a `true` or
        // `false` value for each sortition in the given reward cycle.
        //
        // `cur_tenure_opt` refers to the tenure that is active as of `cur_height`, if there is one.
        // If there is an active tenure in `cur_height`, then if the sortition at `cur_height`
        // matches the `tenure_id_consensus_hash` of `cur_tenure_opt`, `cur_tenure_opt` is
        // set to its parent tenure, and we push `true` to `tenure_status`.  This is the only
        // time we do this, since since `cur_tenure_opt`'s `tenure_id_consensus_hash` only
        // ever matches `cur_consensus_hash` if a tenure began at `cur_height`.  If a tenure did _not_
        // begin at `cur_height`, or if there is no active tenure at `cur_height`, then `tenure_status`.
        // will have `false` for `cur_height`'s bit.
        loop {
            let cur_reward_cycle = sortdb
                .pox_constants
                .block_height_to_reward_cycle(sortdb.first_block_height, cur_height)
                .ok_or(NetError::ChainstateError(
                    "block height comes before system start".into(),
                ))?;
            if cur_reward_cycle < reward_cycle {
                // done scanning this reward cycle
                break;
            }
            let cur_sortition_info = if let Some(info) = self.sortitions.get(&cur_consensus_hash) {
                info
            } else {
                let loaded_info = InvSortitionInfo::load(sortdb, &cur_consensus_hash)?;
                self.sortitions
                    .insert(cur_consensus_hash.clone(), loaded_info);
                self.sortitions
                    .get(&cur_consensus_hash)
                    .expect("infallible: just inserted this data")
            };
            let parent_sortition_consensus_hash = cur_sortition_info.parent_consensus_hash.clone();

            test_debug!("Get sortition and tenure info for height {}. cur_consensus_hash = {}, cur_tenure_info = {:?}, cur_sortition_info = {:?}", cur_height, &cur_consensus_hash, &cur_tenure_opt, cur_sortition_info);

            if let Some(cur_tenure_info) = cur_tenure_opt.as_ref() {
                // a tenure was active when this sortition happened...
                if cur_tenure_info.tenure_id_consensus_hash == cur_consensus_hash {
                    // ...and this tenure started in this sortition
                    tenure_status.push(true);
                    cur_tenure_opt = self.get_processed_tenure(
                        chainstate,
                        &cur_tenure_info.parent_tenure_id_consensus_hash,
                    )?;
                } else {
                    // ...but this tenure did not start in this sortition
                    tenure_status.push(false);
                }
            } else {
                // no active tenure during this sortition. Check the parent sortition to see if a
                // tenure begain there.
                tenure_status.push(false);
                cur_tenure_opt =
                    self.get_processed_tenure(chainstate, &parent_sortition_consensus_hash)?;
            }

            // next sortition
            cur_consensus_hash = parent_sortition_consensus_hash;
            if cur_height == 0 {
                break;
            }
            cur_height = cur_height.saturating_sub(1);
        }

        tenure_status.reverse();
        Ok(tenure_status)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct NakamotoTenureInv {
    /// What state is the machine in?
    pub state: NakamotoInvState,
    /// Bitmap of which tenures a peer has.
    /// Maps reward cycle to bitmap.
    pub tenures_inv: BTreeMap<u64, BitVec<2100>>,
    /// Time of last update, in seconds
    pub last_updated_at: u64,
    /// Burn block height of first sortition
    pub first_block_height: u64,
    /// Length of reward cycle
    pub reward_cycle_len: u64,
    /// Which neighbor is this for
    pub neighbor_address: NeighborAddress,

    /// The fields below are used for synchronizing this particular peer's inventories.
    /// Currently tracked reward cycle
    pub cur_reward_cycle: u64,
    /// Status of this node.
    /// True if we should keep talking to it; false if not
    pub online: bool,
    /// Last time we began talking to this peer
    pub start_sync_time: u64,
}

impl NakamotoTenureInv {
    pub fn new(
        first_block_height: u64,
        reward_cycle_len: u64,
        neighbor_address: NeighborAddress,
    ) -> Self {
        Self {
            state: NakamotoInvState::GetNakamotoInvBegin,
            tenures_inv: BTreeMap::new(),
            last_updated_at: 0,
            first_block_height,
            reward_cycle_len,
            neighbor_address,
            cur_reward_cycle: 0,
            online: true,
            start_sync_time: 0,
        }
    }

    /// Does this remote neighbor have the ith tenure data for the given (absolute) burn block height?
    /// (note that block_height is the _absolute_ block height)
    pub fn has_ith_tenure(&self, burn_block_height: u64) -> bool {
        if burn_block_height < self.first_block_height {
            return false;
        }

        let Some(reward_cycle) = PoxConstants::static_block_height_to_reward_cycle(
            burn_block_height,
            self.first_block_height,
            self.reward_cycle_len,
        ) else {
            return false;
        };

        let Some(rc_tenures) = self.tenures_inv.get(&reward_cycle) else {
            return false;
        };

        let sortition_height = burn_block_height - self.first_block_height;
        let rc_height = u16::try_from(sortition_height % self.reward_cycle_len)
            .expect("FATAL: reward cycle length exceeds u16::MAX");
        rc_tenures.get(rc_height).unwrap_or(false)
    }

    /// How many reward cycles of data do we have for this peer?
    pub fn highest_reward_cycle(&self) -> u64 {
        self.tenures_inv
            .last_key_value()
            .map(|(highest_rc, _)| *highest_rc)
            .unwrap_or(0)
    }

    /// How many blocks are represented in this inv?
    fn num_blocks_represented(&self) -> u64 {
        let mut total = 0;
        for (_, inv) in self.tenures_inv.iter() {
            total += u64::from(inv.len());
        }
        total
    }

    /// Add in a newly-discovered inventory.
    /// NOTE: inventories are supposed to be aligned to the reward cycle
    /// Returns true if we learned about at least one new tenure-start block
    /// Returns false if not.
    pub fn merge_tenure_inv(&mut self, tenure_inv: BitVec<2100>, reward_cycle: u64) -> bool {
        // populate the tenures bitmap to we can fit this tenures inv
        let learned = self
            .tenures_inv
            .get(&reward_cycle)
            .map(|cur_inv| cur_inv != &tenure_inv)
            .unwrap_or(true);

        self.tenures_inv.insert(reward_cycle, tenure_inv);
        self.last_updated_at = get_epoch_time_secs();
        learned
    }

    /// Adjust the next reward cycle to query.
    /// Returns the reward cycle to query.
    pub fn next_reward_cycle(&mut self) -> u64 {
        let query_rc = self.cur_reward_cycle;
        self.cur_reward_cycle = self.cur_reward_cycle.saturating_add(1);
        query_rc
    }

    /// Reset synchronization state for this peer.  Don't remove inventory data; just make it so we
    /// can talk to the peer again
    pub fn try_reset_comms(&mut self, inv_sync_interval: u64, start_rc: u64, cur_rc: u64) {
        let now = get_epoch_time_secs();
        if self.start_sync_time + inv_sync_interval <= now && self.cur_reward_cycle > cur_rc {
            self.state = NakamotoInvState::GetNakamotoInvBegin;
            self.online = true;
            self.start_sync_time = now;
            self.cur_reward_cycle = start_rc;
        }
    }

    /// Get the reward cycle we're sync'ing for
    pub fn reward_cycle(&self) -> u64 {
        self.cur_reward_cycle
    }

    /// Get online status
    pub fn is_online(&self) -> bool {
        self.online
    }

    /// Set online status.  We don't talk to offline peers
    pub fn set_online(&mut self, online: bool) {
        self.online = online;
    }

    /// Proceed to ask this neighbor for its nakamoto tenure inventories.
    /// Returns true if we should proceed to ask for inventories
    /// Returns false if not
    pub fn getnakamotoinv_begin(
        &mut self,
        network: &mut PeerNetwork,
        current_reward_cycle: u64,
    ) -> bool {
        debug!(
            "{:?}: Begin Nakamoto inventory sync for {}",
            network.get_local_peer(),
            self.neighbor_address
        );

        // possibly reset communications with this peer, if it's time to do so.
        self.try_reset_comms(
            network.get_connection_opts().inv_sync_interval,
            current_reward_cycle.saturating_sub(network.get_connection_opts().inv_reward_cycles),
            current_reward_cycle,
        );
        if !self.is_online() {
            // don't talk to this peer for now
            debug!(
                "{:?}: not online: {}",
                network.get_local_peer(),
                &self.neighbor_address
            );
            return false;
        }

        if self.reward_cycle() > current_reward_cycle {
            // we've fully sync'ed with this peer
            debug!(
                "{:?}: fully sync'ed: {}",
                network.get_local_peer(),
                &self.neighbor_address
            );
            return false;
        }

        // ask this neighbor for its inventory
        true
    }

    /// Finish asking for inventories, and update inventory state.
    /// Return Ok(true) if we learned something new
    /// Return Ok(false) if not.
    /// Return Err(..) on I/O errors
    pub fn getnakamotoinv_try_finish(
        &mut self,
        network: &mut PeerNetwork,
        reply: StacksMessage,
    ) -> Result<bool, NetError> {
        match reply.payload {
            StacksMessageType::NakamotoInv(inv_data) => {
                debug!(
                    "{:?}: got NakamotoInv: {:?}",
                    network.get_local_peer(),
                    &inv_data
                );
                let ret = self.merge_tenure_inv(inv_data.tenures, self.reward_cycle());
                self.next_reward_cycle();
                return Ok(ret);
            }
            StacksMessageType::Nack(nack_data) => {
                info!("{:?}: remote peer NACKed our GetNakamotoInv", network.get_local_peer();
                      "error_code" => nack_data.error_code);
                self.set_online(false);
                return Ok(false);
            }
            _ => {
                info!(
                    "{:?}: got unexpected message from {:?}: {:?}",
                    network.get_local_peer(),
                    &self.neighbor_address,
                    &reply
                );
                self.set_online(false);
                return Err(NetError::ConnectionBroken);
            }
        }
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum NakamotoInvState {
    GetNakamotoInvBegin,
    GetNakamotoInvFinish,
    Done,
}

/// Nakamoto inventory state machine
pub struct NakamotoInvStateMachine<NC: NeighborComms> {
    /// Communications links
    pub(crate) comms: NC,
    /// Nakamoto inventories we have
    pub(crate) inventories: HashMap<NeighborAddress, NakamotoTenureInv>,
    /// Reward cycle consensus hashes
    reward_cycle_consensus_hashes: BTreeMap<u64, ConsensusHash>,
}

impl<NC: NeighborComms> NakamotoInvStateMachine<NC> {
    pub fn new(comms: NC) -> Self {
        Self {
            comms,
            inventories: HashMap::new(),
            reward_cycle_consensus_hashes: BTreeMap::new(),
        }
    }

    pub fn reset(&mut self) {
        self.comms.reset();
    }

    /// Remove state for a particular neighbor
    pub fn del_peer(&mut self, peer: &NeighborAddress) {
        self.inventories.remove(peer);
    }

    /// Highest reward cycle learned
    pub fn highest_reward_cycle(&self) -> u64 {
        self.inventories
            .iter()
            .map(|(_, inv)| inv.highest_reward_cycle())
            .max()
            .unwrap_or(0)
    }

    /// Get the consensus hash for the first sortition in the given reward cycle
    fn load_consensus_hash_for_reward_cycle(
        sortdb: &SortitionDB,
        reward_cycle: u64,
    ) -> Result<Option<ConsensusHash>, NetError> {
        let reward_cycle_start_height = sortdb
            .pox_constants
            .reward_cycle_to_block_height(sortdb.first_block_height, reward_cycle);
        let sn = SortitionDB::get_canonical_burn_chain_tip(sortdb.conn())?;
        let ih = sortdb.index_handle(&sn.sortition_id);
        let ch_opt = ih
            .get_block_snapshot_by_height(reward_cycle_start_height)?
            .map(|sn| sn.consensus_hash);
        Ok(ch_opt)
    }

    /// Populate the reward_cycle_consensus_hash mapping.  Idempotent.
    /// Returns the current reward cycle.
    fn update_reward_cycle_consensus_hashes(
        &mut self,
        sortdb: &SortitionDB,
    ) -> Result<u64, NetError> {
        let highest_rc = self
            .reward_cycle_consensus_hashes
            .last_key_value()
            .map(|(highest_rc, _)| *highest_rc)
            .unwrap_or(0);

        let sn = SortitionDB::get_canonical_burn_chain_tip(sortdb.conn())?;
        let tip_rc = sortdb
            .pox_constants
            .block_height_to_reward_cycle(sortdb.first_block_height, sn.block_height)
            .expect("FATAL: snapshot occurred before system start");

        for rc in highest_rc..=tip_rc {
            if self.reward_cycle_consensus_hashes.contains_key(&rc) {
                continue;
            }
            let Some(ch) = Self::load_consensus_hash_for_reward_cycle(sortdb, rc)? else {
                // NOTE: this should be unreachable, but don't panic
                return Err(DBError::NotFoundError.into());
            };
            self.reward_cycle_consensus_hashes.insert(rc, ch);
        }
        Ok(tip_rc)
    }

    /// Make a getnakamotoinv message
    fn make_getnakamotoinv(&self, reward_cycle: u64) -> Option<StacksMessageType> {
        let Some(ch) = self.reward_cycle_consensus_hashes.get(&reward_cycle) else {
            return None;
        };
        Some(StacksMessageType::GetNakamotoInv(GetNakamotoInvData {
            consensus_hash: ch.clone(),
        }))
    }

    /// Proceed to ask neighbors for their nakamoto tenure inventories.
    /// If we're in initial block download (ibd), then only ask our bootstrap peers.
    /// Otherwise, ask everyone.
    /// Returns Err(..) on I/O errors
    pub fn process_getnakamotoinv_begins(
        &mut self,
        network: &mut PeerNetwork,
        sortdb: &SortitionDB,
        ibd: bool,
    ) -> Result<(), NetError> {
        // make sure we know all consensus hashes for all reward cycles.
        let current_reward_cycle = self.update_reward_cycle_consensus_hashes(sortdb)?;

        // we're updating inventories, so preserve the state we have
        let mut new_inventories = HashMap::new();
        let event_ids: Vec<usize> = network.iter_peer_event_ids().map(|e_id| *e_id).collect();
        for event_id in event_ids.into_iter() {
            let Some(convo) = network.get_p2p_convo(event_id) else {
                continue;
            };
            if !convo.is_outbound() || !convo.is_authenticated() {
                continue;
            }
            if ibd {
                // in IBD, only connect to initial peers
                let is_initial = PeerDB::is_initial_peer(
                    &network.peerdb_conn(),
                    convo.peer_network_id,
                    &convo.peer_addrbytes,
                    convo.peer_port,
                )
                .unwrap_or(false);
                if !is_initial {
                    continue;
                }
            }

            let naddr = convo.to_neighbor_address();

            // NOTE: this naturally garabage-collects inventories for disconnected nodes, as
            // desired
            let mut inv = self.inventories.remove(&naddr).unwrap_or_else(|| {
                NakamotoTenureInv::new(
                    network.get_burnchain().first_block_height,
                    network
                        .get_burnchain()
                        .pox_constants
                        .reward_cycle_length
                        .into(),
                    naddr.clone(),
                )
            });

            let proceed = inv.getnakamotoinv_begin(network, current_reward_cycle);
            let inv_rc = inv.reward_cycle();
            new_inventories.insert(naddr.clone(), inv);

            if self.comms.has_inflight(&naddr) {
                continue;
            }

            if !proceed {
                continue;
            }

            // ask this neighbor for its inventory
            let Some(getnakamotoinv) = self.make_getnakamotoinv(inv_rc) else {
                continue;
            };

            debug!(
                "{:?}: send GetNakamotoInv for reward cycle {} to {}",
                network.get_local_peer(),
                inv_rc,
                &naddr
            );

            if let Err(e) = self.comms.neighbor_send(network, &naddr, getnakamotoinv) {
                warn!("{:?}: failed to send GetNakamotoInv", network.get_local_peer();
                      "peer" => ?naddr,
                      "error" => ?e
                );
            }
        }

        self.inventories = new_inventories;
        Ok(())
    }

    /// Finish asking for inventories, and update inventory state.
    /// Returns Ok(num-messages, true) if an inv state machine learned something.
    /// Returns Ok(num-messages, false) if not
    /// Returns Err(..) on I/O errors
    pub fn process_getnakamotoinv_finishes(
        &mut self,
        network: &mut PeerNetwork,
    ) -> Result<(usize, bool), NetError> {
        let mut learned = false;
        let replies = self.comms.collect_replies(network);
        let num_msgs = replies.len();

        for (naddr, reply) in replies.into_iter() {
            test_debug!(
                "{:?}: got reply from {}: {:?}",
                network.get_local_peer(),
                &naddr,
                &reply
            );
            let Some(inv) = self.inventories.get_mut(&naddr) else {
                debug!(
                    "{:?}: Got a reply for an untracked inventory peer {}: {:?}",
                    network.get_local_peer(),
                    &naddr,
                    &reply
                );
                continue;
            };

            let Ok(inv_learned) = inv.getnakamotoinv_try_finish(network, reply).map_err(|e| {
                warn!(
                    "{:?}: Failed to finish inventory sync to {}: {:?}",
                    network.get_local_peer(),
                    &naddr,
                    &e
                );
                self.comms.add_broken(network, &naddr);
                e
            }) else {
                continue;
            };

            learned = learned || inv_learned;
        }

        Ok((num_msgs, learned))
    }

    pub fn run(&mut self, network: &mut PeerNetwork, sortdb: &SortitionDB, ibd: bool) -> bool {
        if let Err(e) = self.process_getnakamotoinv_begins(network, sortdb, ibd) {
            warn!(
                "{:?}: Failed to begin Nakamoto tenure inventory sync: {:?}",
                network.get_local_peer(),
                &e
            );
        }
        let Ok((_, learned)) = self.process_getnakamotoinv_finishes(network).map_err(|e| {
            warn!(
                "{:?}: Failed to finish Nakamoto tenure inventory sync: {:?}",
                network.get_local_peer(),
                &e
            );
            e
        }) else {
            return false;
        };
        learned
    }
}

impl PeerNetwork {
    /// Initialize inv state for nakamoto
    pub fn init_inv_sync_nakamoto(&mut self) {
        // find out who we'll be synchronizing with for the duration of this inv sync
        debug!(
            "{:?}: Initializing peer block inventory state for Nakamoto",
            &self.local_peer,
        );
        self.inv_state_nakamoto = Some(NakamotoInvStateMachine::new(PeerNetworkComms::new()));
    }

    /// Drive Nakamoto inventory state machine
    /// returns (learned-new-data?, peers-to-disconnect, peers-that-are-dead)
    pub fn sync_inventories_nakamoto(
        &mut self,
        sortdb: &SortitionDB,
        ibd: bool,
    ) -> (bool, Vec<NeighborKey>, Vec<NeighborKey>) {
        if self.inv_state_nakamoto.is_none() {
            self.init_inv_sync_nakamoto();
        }
        let Some(mut nakamoto_inv) = self.inv_state_nakamoto.take() else {
            return (false, vec![], vec![]);
        };

        let learned = nakamoto_inv.run(self, sortdb, ibd);
        let dead = nakamoto_inv.comms.take_dead_neighbors();
        let broken = nakamoto_inv.comms.take_broken_neighbors();

        self.inv_state_nakamoto = Some(nakamoto_inv);

        (
            learned,
            dead.into_iter().collect(),
            broken.into_iter().collect(),
        )
    }

    /// Update the state of our neighbors' Nakamoto tenure inventories
    /// Return whether or not we learned something
    pub fn do_network_inv_sync_nakamoto(&mut self, sortdb: &SortitionDB, ibd: bool) -> bool {
        if cfg!(test) && self.connection_opts.disable_inv_sync {
            test_debug!("{:?}: inv sync is disabled", &self.local_peer);
            return false;
        }

        debug!(
            "{:?}: network inventory sync for Nakamoto",
            &self.local_peer
        );

        if self.inv_state_nakamoto.is_none() {
            self.init_inv_sync_nakamoto();
        }

        // synchronize peer block inventories
        let (learned, dead_neighbors, broken_neighbors) =
            self.sync_inventories_nakamoto(sortdb, ibd);

        // disconnect and ban broken peers
        for broken in broken_neighbors.into_iter() {
            self.deregister_and_ban_neighbor(&broken);
        }

        // disconnect from dead connections
        for dead in dead_neighbors.into_iter() {
            self.deregister_neighbor(&dead);
        }

        learned
    }
}
