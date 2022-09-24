// Copyright 2022 Parity Technologies (UK) Ltd.
// This file is part of Polkadot.

// Polkadot is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Polkadot is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Polkadot.  If not, see <http://www.gnu.org/licenses/>.

//! Direct distribution of statements, even those concerning candidates which
//! are not yet backed.
//!
//! Members of a validation group assigned to a para at a given relay-parent
//! always distribute statements directly to each other.
//!
//! Each validator in the group is permitted to send up to some limit of
//! `Seconded` statements per validator in the group. These may differ per-validator,
//! if an attacker is exploiting network partitions, so we have to track up to
//! `limit*group_size^2` `Seconded` statements. The limits and group sizes are both
//! relatively small, and this is an absolute worst case.
//!
//! This module exposes a "DirectInGroup" utility which allows us to determine
//! whether to accept or reject messages, and to track which candidates we consider
//! 'legitimate' based on the first `limit` `Seconded` statements we see signed by
//! each validator.
// TODO [now]: decide if we want to also distribute statements to validators
// that are assigned as-of an active leaf i.e. the next group.

use std::ops::Range;

use polkadot_primitives::vstaging::{ValidatorIndex, CandidateHash};

/// Utility for keeping track of limits on direct statements within a group.
///
/// See module docs for more details.
pub struct DirectInGroup {
	validators: Vec<ValidatorIndex>,
	our_index: usize,
	seconding_limit: usize,

	// a 3D matrix where the dimensions have the following meaning
	// X: indicates the sending validator (size: group_size - 1, omitting self)
	// Y: indicates the originating validator who issued the statement (size: group_size)
	// Z: the candidate hash of the statement (size: seconding_limit)
	//
	// preallocated to (group size - 1) * group_size * seconding_limit.
	incoming: Vec<Option<CandidateHash>>,

	// a 2D matrix of accepted incoming `Seconded` messages from validators
	// in the group.
	// X: indicates the originating validator (size: group_size)
	// Y: a seconded candidate we've accepted knowledge of locally (size: seconding_limit)
	accepted: Vec<Option<CandidateHash>>,

	// TODO [now]: outgoing sends
}

impl DirectInGroup {
	/// Instantiate a new `DirectInGroup` tracker. Fails if `our_index` is out of bounds
	/// or `group_validators` is empty or `our_index` is not in the group.
	pub fn new(
		group_validators: Vec<ValidatorIndex>,
		our_index: ValidatorIndex,
		seconding_limit: usize,
	) -> Option<Self> {
		if group_validators.is_empty() { return None }
		if our_index.0 as usize >= group_validators.len() { return None }

		let our_index = index_in_group(&group_validators, our_index)?;

		let incoming_size = (group_validators.len() - 1) * group_validators.len() * seconding_limit;
		let accepted_size = group_validators.len() * seconding_limit;

		let incoming = vec![None; incoming_size];
		let accepted = vec![None; accepted_size];

		Some(DirectInGroup {
			validators: group_validators,
			our_index,
			seconding_limit,
			incoming,
			accepted,
		})
	}

	/// Handle an incoming `Seconded` statement from the given validator.
	/// If the outcome is `Reject` then no internal state is altered.
	pub fn handle_incoming_seconded(
		&mut self,
		sender: ValidatorIndex,
		originator: ValidatorIndex,
		candidate_hash: CandidateHash,
	) -> Result<AcceptIncoming, RejectIncoming> {
		let sender_index = match self.index_in_group(sender) {
			None => return Err(RejectIncoming::NotInGroup),
			Some(i) => i,
		};

		let originator_index = match self.index_in_group(sender) {
			None => return Err(RejectIncoming::NotInGroup),
			Some(i) => i,
		};

		if sender_index == self.our_index || originator_index == self.our_index {
			return Err(RejectIncoming::NotInGroup);
		}

		let range = self.incoming_range(sender_index, originator_index);
		for i in range {
			if self.incoming[i] == Some(candidate_hash) {
				// duplicates get rejected.
				return Err(RejectIncoming::PeerExcess)
			}

			// ok, found an empty slot.
			if self.incoming[i].is_none() {
				self.incoming[i] = Some(candidate_hash);
				return self.handle_accepted_incoming(
					originator_index,
					candidate_hash,
				);
			}
		}

		Err(RejectIncoming::PeerExcess)
	}

	// TODO [now]: some API analogues to can_send / can_receive.

	fn handle_accepted_incoming(
		&mut self,
		originator: usize,
		candidate_hash: CandidateHash,
	) -> Result<AcceptIncoming, RejectIncoming> {
		let range = self.accepted_range(originator);
		for i in range {
			if self.accepted[i] == Some(candidate_hash) {
				return Ok(AcceptIncoming::YesKnown);
			}

			if self.accepted[i].is_none() {
				self.accepted[i] = Some(candidate_hash);
				return Ok(AcceptIncoming::YesUnknown);
			}
		}

		Err(RejectIncoming::OriginatorExcess)
	}

	fn index_in_group(&self, validator: ValidatorIndex) -> Option<usize> {
		index_in_group(&self.validators, validator)
	}

	fn adjust_for_skipped_self(&self, index: usize) -> usize {
		if index > self.our_index { index - 1 } else { index }
	}

	fn incoming_range(&self, sender: usize, originator: usize) -> Range<usize> {
		// adjust X dimension to account for the fact that our index is skipped.
		let sender = self.adjust_for_skipped_self(sender);
		let base = (sender * (self.validators.len() - 1)) + originator * self.seconding_limit;

		base .. base + self.seconding_limit
	}

	fn accepted_range(&self, originator: usize) -> Range<usize> {
		let base = originator * self.seconding_limit;
		base .. base + self.seconding_limit
	}
}

/// Incoming `Seconded` message was rejected.
pub enum RejectIncoming {
	/// Peer sent excessive messages.
	PeerExcess,
	/// Originator sent excessive messages, peer seems innocent.
	OriginatorExcess,
	/// Sender or originator is not in the group.
	NotInGroup,
}

/// Incoming `Seconded` message was accepted.
pub enum AcceptIncoming {
	/// The `Seconded` statement was within the peer's limits and unknown
	/// for the originator.
	YesUnknown,
	/// The `Seconded` statement was within the peer's limits and already
	/// known for the originator.
	YesKnown,
}

fn index_in_group(validators: &[ValidatorIndex], index: ValidatorIndex) -> Option<usize> {
	validators.iter().position(|v| v == &index)
}
