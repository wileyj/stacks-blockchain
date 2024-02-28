use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::ToSql;
use serde::{Deserialize, Serialize};

use crate::codec::{
    read_next, read_next_exact, write_next, Error as CodecError, StacksMessageCodec,
};
use crate::util::hash::{bytes_to_hex, hex_bytes};

#[derive(Clone, PartialEq, Eq, Debug)]
/// This data structure represents a list of booleans
/// as a bitvector.
///
/// The generic argument `MAX_SIZE` specifies the maximum number of
/// elements that the bit vector can hold. It is not the _actual_ size
/// of the bitvec: if there are only 8 entries, the bitvector will
/// just have a single byte, even if the MAX_SIZE is u16::MAX. This
/// type parameter ensures that constructors and deserialization routines
/// error if input data is too long.
pub struct BitVec<const MAX_SIZE: u16> {
    data: Vec<u8>,
    len: u16,
}

impl<const MAX_SIZE: u16> TryFrom<&[bool]> for BitVec<MAX_SIZE> {
    type Error = String;

    fn try_from(value: &[bool]) -> Result<Self, Self::Error> {
        let len = value
            .len()
            .try_into()
            .map_err(|_| "BitVec length must be u16")?;
        if len == 0 {
            return Err("BitVec length must be positive".into());
        }
        if len > MAX_SIZE {
            return Err(format!(
                "BitVec length is too long. Max size = {MAX_SIZE}, Input len = {len}"
            ));
        }
        let mut bitvec = BitVec::zeros(len)?;
        for (ix, bool_value) in value.iter().enumerate() {
            let ix = ix.try_into().map_err(|_| "BitVec length must be u16")?;
            // only need to set the bitvec value if `bool_value` is true,
            // because we initialized with zeros
            if *bool_value {
                bitvec.set(ix, true)?;
            }
        }
        Ok(bitvec)
    }
}

impl<const MAX_SIZE: u16> StacksMessageCodec for BitVec<MAX_SIZE> {
    fn consensus_serialize<W: std::io::Write>(&self, fd: &mut W) -> Result<(), CodecError> {
        write_next(fd, &self.len)?;
        write_next(fd, &self.data)
    }

    fn consensus_deserialize<R: std::io::Read>(fd: &mut R) -> Result<Self, CodecError> {
        let len = read_next(fd)?;
        if len == 0 {
            return Err(CodecError::DeserializeError(
                "BitVec lengths must be positive".to_string(),
            ));
        }
        if len > MAX_SIZE {
            return Err(CodecError::DeserializeError(format!(
                "BitVec length exceeded maximum. Max size = {MAX_SIZE}, len = {len}"
            )));
        }

        let data = read_next_exact(fd, Self::data_len(len).into())?;
        Ok(BitVec { data, len })
    }
}

impl<const MAX_SIZE: u16> Serialize for BitVec<MAX_SIZE> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let hex = bytes_to_hex(self.serialize_to_vec().as_slice());
        serializer.serialize_str(&hex)
    }
}

impl<'de, const MAX_SIZE: u16> Deserialize<'de> for BitVec<MAX_SIZE> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex: &str = Deserialize::deserialize(deserializer)?;
        let bytes = hex_bytes(hex).map_err(serde::de::Error::custom)?;
        Self::consensus_deserialize(&mut bytes.as_slice()).map_err(serde::de::Error::custom)
    }
}

impl<const MAX_SIZE: u16> FromSql for BitVec<MAX_SIZE> {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let bytes = hex_bytes(value.as_str()?).map_err(|e| FromSqlError::Other(Box::new(e)))?;
        Self::consensus_deserialize(&mut bytes.as_slice())
            .map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}

impl<const MAX_SIZE: u16> ToSql for BitVec<MAX_SIZE> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        let hex = bytes_to_hex(self.serialize_to_vec().as_slice());
        Ok(hex.into())
    }
}

impl<const MAX_SIZE: u16> BitVec<MAX_SIZE> {
    /// Construct a new BitVec with all entries set to `false` and total length `len`
    pub fn zeros(len: u16) -> Result<BitVec<MAX_SIZE>, String> {
        if len > MAX_SIZE {
            return Err(format!(
                "BitVec length is too long. Max size = {MAX_SIZE}, Input len = {len}"
            ));
        }
        let data = vec![0; usize::from(Self::data_len(len))];
        Ok(BitVec { data, len })
    }

    pub fn len(&self) -> u16 {
        self.len
    }

    fn data_len(len: u16) -> u16 {
        len / 8 + if len % 8 == 0 { 0 } else { 1 }
    }

    /// Get a u8 with the (index % 8)th bit set to 1.
    fn bit_index(index: u16) -> u8 {
        1 << u8::try_from(index % 8).expect("FATAL: remainder 8 returned a non-u8 value")
    }

    pub fn get(&self, i: u16) -> Option<bool> {
        if i >= self.len {
            return None;
        }
        let vec_index = usize::from(i / 8);
        let byte = self.data.get(vec_index)?;
        let bit_index = Self::bit_index(i);
        Some((*byte & bit_index) != 0)
    }

    pub fn set(&mut self, i: u16, val: bool) -> Result<(), String> {
        if i >= self.len {
            return Err(format!(
                "Index `{i}` outside of bitvec length `{}`",
                self.len
            ));
        }
        let vec_index = usize::from(i / 8);
        let Some(byte) = self.data.get_mut(vec_index) else {
            return Err(format!(
                "Index `{i}/8` outside of bitvec data length `{}`",
                self.data.len()
            ));
        };
        let bit_index = Self::bit_index(i);
        if val {
            *byte |= bit_index;
        } else {
            *byte &= !bit_index;
        }
        Ok(())
    }

    /// Set all bits to zero
    pub fn clear(&mut self) {
        for i in 0..self.data.len() {
            self.data[i] = 0;
        }
    }
}

#[cfg(test)]
mod test {
    use super::BitVec;
    use crate::codec::StacksMessageCodec;

    fn check_set_get(mut input: BitVec<{ u16::MAX }>) {
        let original_input = input.clone();
        for i in 0..input.len() {
            let original_value = input.get(i).unwrap();
            input.set(i, false).unwrap();
            assert_eq!(input.len(), original_input.len());
            for j in 0..input.len() {
                if j == i {
                    continue;
                }
                assert_eq!(original_input.get(j), input.get(j));
            }
            assert_eq!(input.get(i), Some(false));
            input.set(i, true).unwrap();
            for j in 0..input.len() {
                if j == i {
                    continue;
                }
                assert_eq!(original_input.get(j), input.get(j));
            }
            assert_eq!(input.get(i), Some(true));
            input.set(i, original_value).unwrap();
            assert_eq!(input.get(i), Some(original_value));
        }
        assert_eq!(input, original_input);
        assert!(input.set(input.len(), false).is_err());
    }

    fn check_serialization(input: &BitVec<{ u16::MAX }>) {
        let byte_ser = input.serialize_to_vec();
        let deserialized = BitVec::consensus_deserialize(&mut byte_ser.as_slice()).unwrap();
        assert_eq!(input, &deserialized);
    }

    fn check_ok_vector(input: &[bool]) {
        let bitvec = BitVec::try_from(input).unwrap();
        assert_eq!(bitvec.len(), input.len() as u16);
        for (ix, value) in input.iter().enumerate() {
            assert_eq!(bitvec.get(u16::try_from(ix).unwrap()), Some(*value));
        }
        // check that a length check will fail
        let passed_len_2_check = BitVec::<2>::try_from(input).is_ok();
        if input.len() <= 2 {
            assert!(
                passed_len_2_check,
                "BitVec should pass assembly in length-2 max because input is length-2"
            );
        } else {
            assert!(!passed_len_2_check, "BitVec should fail assembly in length-2 max because input is greater that length-2");
        }
        // check that a length check will fail on deserialization
        let serialization = bitvec.serialize_to_vec();
        let passed_len_2_deser =
            BitVec::<2>::consensus_deserialize(&mut serialization.as_slice()).is_ok();
        if input.len() <= 2 {
            assert!(
                passed_len_2_deser,
                "BitVec should pass assembly in length-2 max because input is length-2"
            );
        } else {
            assert!(!passed_len_2_deser, "BitVec should fail assembly in length-2 max because input is greater that length-2");
        }

        check_serialization(&bitvec);
        check_set_get(bitvec);
    }

    #[test]
    fn zeros_constructor() {
        let bitvec_zero_10 = BitVec::<10>::zeros(10).unwrap();
        for i in 0..10 {
            assert!(
                !bitvec_zero_10.get(i).unwrap(),
                "All values of zero vec should be false"
            );
        }
        assert!(
            BitVec::<2>::zeros(3).is_err(),
            "Should fail to construct a length 3 zero vec when bound to bitlength 2"
        );
    }

    #[test]
    fn vectors() {
        let mut inputs = vec![
            vec![true; 8],
            vec![false; 8],
            vec![true; 12],
            vec![false; 12],
            vec![false],
            vec![true],
            vec![false, true],
            vec![true, false],
        ];
        for i in 0..8 {
            let mut single_set_vec = vec![false; 8];
            let mut single_unset_vec = vec![true; 8];
            single_unset_vec[i] = false;
            single_set_vec[i] = true;
            inputs.push(single_set_vec);
            inputs.push(single_unset_vec);
        }
        let large_set_vec = vec![false; u16::MAX.into()];
        let large_unset_vec = vec![true; u16::MAX.into()];
        inputs.push(large_set_vec);
        inputs.push(large_unset_vec);

        for i in 1..128 {
            let mut bool_vec = vec![false; i];
            for (j, val) in bool_vec.iter_mut().enumerate() {
                *val = j % 2 == 0;
            }
            inputs.push(bool_vec);
        }

        for i in inputs.into_iter() {
            check_ok_vector(i.as_slice());
        }
    }
}
