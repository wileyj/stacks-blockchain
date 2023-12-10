// Copyright (C) 2020 Stacks Open Internet Foundation
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

use clarity::vm::errors::RuntimeErrorType as ClarityRuntimeError;
use clarity::vm::types::{PrincipalData, QualifiedContractIdentifier, StandardPrincipalData};
use clarity::vm::ContractName;
use stacks_common::codec::StacksMessageCodec;

use crate::burnchains::{BurnchainBlockHeader, BurnchainTransaction};
use crate::chainstate::burn::operations::{Error as OpError, PegInOp};
use crate::chainstate::burn::Opcodes;
use crate::types::chainstate::StacksAddress;
use crate::types::Address;

/// Transaction structure:
///
/// Output 0: data output (see PegInOp::parse_data())
/// Output 1: payment to peg wallet address
///
impl PegInOp {
    pub fn from_tx(
        block_header: &BurnchainBlockHeader,
        tx: &BurnchainTransaction,
    ) -> Result<Self, OpError> {
        if tx.opcode() != Opcodes::PegIn as u8 {
            warn!("Invalid tx: invalid opcode {}", tx.opcode());
            return Err(OpError::InvalidInput);
        }

        let (amount, peg_wallet_address) =
            if let Some(Some(recipient)) = tx.get_recipients().first() {
                (recipient.amount, recipient.address.clone())
            } else {
                warn!("Invalid tx: First output not recognized");
                return Err(OpError::InvalidInput);
            };

        let parsed_data = Self::parse_data(&tx.data())?;

        let txid = tx.txid();
        let vtxindex = tx.vtxindex();
        let block_height = block_header.block_height;
        let burn_header_hash = block_header.block_hash;

        Ok(Self {
            recipient: parsed_data.recipient,
            peg_wallet_address,
            amount,
            memo: parsed_data.memo,
            txid,
            vtxindex,
            block_height,
            burn_header_hash,
        })
    }

    fn parse_data(data: &[u8]) -> Result<ParsedData, ParseError> {
        /*
            Wire format:

            0      2  3                  24                            64       80
            |------|--|------------------|-----------------------------|--------|
             magic  op   Stacks address      Contract name (optional)     memo

             Note that `data` is missing the first 3 bytes -- the magic and op must
             be stripped before this method is called. At the time of writing,
             this is done in `burnchains::bitcoin::blocks::BitcoinBlockParser::parse_data`.
        */

        if data.len() < 21 {
            warn!(
                "PegInOp payload is malformed ({} bytes, expected at least {})",
                data.len(),
                21
            );
            return Err(ParseError::MalformedData);
        }

        let version = *data.get(0).expect("No version byte");
        let address_data: [u8; 20] = data
            .get(1..21)
            .ok_or(ParseError::MalformedData)?
            .try_into()?;

        let standard_principal_data = StandardPrincipalData(version, address_data);

        let memo = data.get(61..).unwrap_or(&[]).to_vec();

        let recipient: PrincipalData =
            if let Some(contract_bytes) = Self::leading_non_zero_bytes(data, 21, 61) {
                let contract_name: String = std::str::from_utf8(contract_bytes)?.to_owned();

                QualifiedContractIdentifier::new(standard_principal_data, contract_name.try_into()?)
                    .into()
            } else {
                standard_principal_data.into()
            };

        Ok(ParsedData { recipient, memo })
    }

    pub fn check(&self) -> Result<(), OpError> {
        if self.amount == 0 {
            warn!("PEG_IN Invalid: Peg amount must be positive");
            return Err(OpError::AmountMustBePositive);
        }

        Ok(())
    }

    /// Returns the leading non-zero bytes of the subslice `data[from..to]`
    ///
    /// # Panics
    ///
    /// Panics if `from` is larger than or equal to `to`
    fn leading_non_zero_bytes(data: &[u8], from: usize, to: usize) -> Option<&[u8]> {
        assert!(from < to);

        let end_of_non_zero_slice = {
            let mut end = to.min(data.len());
            for i in from..end {
                if data[i] == 0 {
                    end = i;
                    break;
                }
            }
            end
        };

        if from == end_of_non_zero_slice {
            return None;
        }

        data.get(from..end_of_non_zero_slice)
    }
}

struct ParsedData {
    recipient: PrincipalData,
    memo: Vec<u8>,
}

enum ParseError {
    BadContractName,
    MalformedData,
    Utf8Error,
}

impl From<ParseError> for OpError {
    fn from(_: ParseError) -> Self {
        Self::ParseError
    }
}

impl From<std::str::Utf8Error> for ParseError {
    fn from(_: std::str::Utf8Error) -> Self {
        Self::Utf8Error
    }
}

impl From<std::array::TryFromSliceError> for ParseError {
    fn from(_: std::array::TryFromSliceError) -> Self {
        Self::MalformedData
    }
}

impl From<ClarityRuntimeError> for ParseError {
    fn from(_: ClarityRuntimeError) -> Self {
        Self::BadContractName
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chainstate::burn::operations::test;

    #[test]
    fn test_parse_peg_in_should_succeed_given_a_conforming_transaction_without_memo() {
        let mut rng = test::seeded_rng();
        let opcode = Opcodes::PegIn;

        let peg_wallet_address = test::random_bytes(&mut rng);
        let amount = 10;
        let output2 = test::Output::new(amount, peg_wallet_address);

        let mut data = vec![1];
        let addr_bytes = test::random_bytes(&mut rng);
        let stx_address = StacksAddress::new(1, addr_bytes.into());
        data.extend_from_slice(&addr_bytes);

        let tx = test::burnchain_transaction(data, Some(output2), opcode);
        let header = test::burnchain_block_header();

        let op = PegInOp::from_tx(&header, &tx).expect("Failed to construct peg-in operation");

        assert_eq!(op.recipient, stx_address.into());
        assert_eq!(op.amount, amount);
        assert_eq!(op.peg_wallet_address.bytes(), peg_wallet_address);
    }

    #[test]
    fn test_parse_peg_in_should_succeed_given_a_conforming_transaction_with_memo() {
        let mut rng = test::seeded_rng();
        let opcode = Opcodes::PegIn;

        let peg_wallet_address = test::random_bytes(&mut rng);
        let amount = 10;
        let output2 = test::Output::new(amount, peg_wallet_address);
        let memo: [u8; 6] = test::random_bytes(&mut rng);

        let mut data = vec![1];
        let addr_bytes = test::random_bytes(&mut rng);
        let stx_address = StacksAddress::new(1, addr_bytes.into());
        data.extend_from_slice(&addr_bytes);
        data.extend_from_slice(&[0; 40]); // Padding contract name
        data.extend_from_slice(&memo);

        let tx = test::burnchain_transaction(data, Some(output2), opcode);
        let header = test::burnchain_block_header();

        let op = PegInOp::from_tx(&header, &tx).expect("Failed to construct peg-in operation");

        assert_eq!(op.recipient, stx_address.into());
        assert_eq!(op.amount, amount);
        assert_eq!(op.peg_wallet_address.bytes(), peg_wallet_address);
        assert_eq!(op.memo.as_slice(), memo)
    }

    #[test]
    fn test_parse_peg_in_should_succeed_given_a_contract_recipient() {
        let mut rng = test::seeded_rng();
        let opcode = Opcodes::PegIn;

        let contract_name = "This_is_a_valid_contract_name";
        let peg_wallet_address = test::random_bytes(&mut rng);
        let amount = 10;
        let output2 = test::Output::new(amount, peg_wallet_address);
        let memo: [u8; 6] = test::random_bytes(&mut rng);

        let mut data = vec![1];
        let addr_bytes = test::random_bytes(&mut rng);
        let stx_address = StacksAddress::new(1, addr_bytes.into());
        data.extend_from_slice(&addr_bytes);
        data.extend_from_slice(contract_name.as_bytes());
        data.extend_from_slice(&[0; 11]); // Padding contract name
        data.extend_from_slice(&memo);

        let tx = test::burnchain_transaction(data, Some(output2), opcode);
        let header = test::burnchain_block_header();

        let op = PegInOp::from_tx(&header, &tx).expect("Failed to construct peg-in operation");

        let expected_principal =
            QualifiedContractIdentifier::new(stx_address.into(), contract_name.into()).into();

        assert_eq!(op.recipient, expected_principal);
        assert_eq!(op.amount, amount);
        assert_eq!(op.peg_wallet_address.bytes(), peg_wallet_address);
        assert_eq!(op.memo.as_slice(), memo)
    }

    #[test]
    fn test_parse_peg_in_should_return_error_given_invalid_contract_name() {
        let mut rng = test::seeded_rng();
        let opcode = Opcodes::PegIn;

        let contract_name = "Mårten_is_not_a_valid_contract_name";
        let peg_wallet_address = test::random_bytes(&mut rng);
        let amount = 10;
        let output2 = test::Output::new(amount, peg_wallet_address);
        let memo: [u8; 6] = test::random_bytes(&mut rng);

        let mut data = vec![1];
        let addr_bytes = test::random_bytes(&mut rng);
        let stx_address = StacksAddress::new(1, addr_bytes.into());
        data.extend_from_slice(&addr_bytes);
        data.extend_from_slice(contract_name.as_bytes());
        data.extend_from_slice(&[0; 4]); // Padding contract name
        data.extend_from_slice(&memo);

        let tx = test::burnchain_transaction(data, Some(output2), opcode);
        let header = test::burnchain_block_header();

        let op = PegInOp::from_tx(&header, &tx);

        match op {
            Err(OpError::ParseError) => (),
            result => panic!("Expected OpError::ParseError, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_peg_in_should_return_error_given_wrong_opcode() {
        let mut rng = test::seeded_rng();
        let opcode = Opcodes::StackStx;

        let peg_wallet_address = test::random_bytes(&mut rng);
        let amount = 10;

        let output2 = test::Output::new(amount, peg_wallet_address);
        let memo: [u8; 6] = test::random_bytes(&mut rng);

        let mut data = vec![1];
        let addr_bytes: [u8; 20] = test::random_bytes(&mut rng);
        data.extend_from_slice(&addr_bytes);
        data.extend_from_slice(&[0; 40]); // Padding contract name
        data.extend_from_slice(&memo);

        let tx = test::burnchain_transaction(data, Some(output2), opcode);
        let header = test::burnchain_block_header();

        let op = PegInOp::from_tx(&header, &tx);

        match op {
            Err(OpError::InvalidInput) => (),
            result => panic!("Expected OpError::InvalidInput, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_peg_in_should_return_error_given_invalid_utf8_contract_name() {
        let invalid_utf8_byte_sequence = [255, 255];

        let mut rng = test::seeded_rng();
        let opcode = Opcodes::PegIn;

        let peg_wallet_address = test::random_bytes(&mut rng);
        let amount = 10;
        let output2 = test::Output::new(amount, peg_wallet_address);
        let memo: [u8; 6] = test::random_bytes(&mut rng);

        let mut data = vec![1];
        let addr_bytes: [u8; 20] = test::random_bytes(&mut rng);
        data.extend_from_slice(&addr_bytes);
        data.extend_from_slice(&invalid_utf8_byte_sequence);
        data.extend_from_slice(&[0; 40]); // Padding contract name
        data.extend_from_slice(&memo);

        let tx = test::burnchain_transaction(data, Some(output2), opcode);
        let header = test::burnchain_block_header();

        let op = PegInOp::from_tx(&header, &tx);

        match op {
            Err(OpError::ParseError) => (),
            result => panic!("Expected OpError::ParseError, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_peg_in_should_return_error_given_no_second_output() {
        let mut rng = test::seeded_rng();
        let opcode = Opcodes::PegIn;

        let memo: [u8; 6] = test::random_bytes(&mut rng);

        let mut data = vec![1];
        let addr_bytes: [u8; 20] = test::random_bytes(&mut rng);
        data.extend_from_slice(&addr_bytes);
        data.extend_from_slice(&[0; 40]); // Padding contract name
        data.extend_from_slice(&memo);

        let tx = test::burnchain_transaction(data, None, opcode);
        let header = test::burnchain_block_header();

        let op = PegInOp::from_tx(&header, &tx);

        match op {
            Err(OpError::InvalidInput) => (),
            result => panic!("Expected OpError::InvalidInput, got {:?}", result),
        }
    }

    #[test]
    fn test_parse_peg_in_should_return_error_given_too_short_data_array() {
        let mut rng = test::seeded_rng();
        let opcode = Opcodes::PegIn;

        let peg_wallet_address = test::random_bytes(&mut rng);
        let amount = 10;
        let output2 = test::Output::new(amount, peg_wallet_address);

        let mut data = vec![1];
        let addr_bytes: [u8; 19] = test::random_bytes(&mut rng);
        data.extend_from_slice(&addr_bytes);

        let tx = test::burnchain_transaction(data, Some(output2), opcode);
        let header = test::burnchain_block_header();

        let op = PegInOp::from_tx(&header, &tx);

        match op {
            Err(OpError::ParseError) => (),
            result => panic!("Expected OpError::InvalidInput, got {:?}", result),
        }
    }

    #[test]
    fn test_check_should_return_error_on_zero_amount_and_ok_on_any_other_values() {
        let mut rng = test::seeded_rng();

        let peg_wallet_address = test::random_bytes(&mut rng);
        let memo: [u8; 6] = test::random_bytes(&mut rng);

        let mut data = vec![1];
        let addr_bytes = test::random_bytes(&mut rng);
        let stx_address = StacksAddress::new(1, addr_bytes.into());
        data.extend_from_slice(&addr_bytes);
        data.extend_from_slice(&[0; 40]); // Padding contract name
        data.extend_from_slice(&memo);

        let create_op = move |amount| {
            let opcode = Opcodes::PegIn;
            let output2 = test::Output::new(amount, peg_wallet_address.clone());

            let tx = test::burnchain_transaction(data.clone(), Some(output2), opcode);
            let header = test::burnchain_block_header();

            PegInOp::from_tx(&header, &tx).expect("Failed to construct peg-in operation")
        };

        match create_op(0).check() {
            Err(OpError::AmountMustBePositive) => (),
            result => panic!(
                "Expected OpError::PegInAmountMustBePositive, got {:?}",
                result
            ),
        };

        create_op(1)
            .check()
            .expect("Any strictly positive amounts should be ok");

        create_op(u64::MAX)
            .check()
            .expect("Any strictly positive amounts should be ok");
    }
}
