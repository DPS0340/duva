//! A module providing variable-length size encoding followed by arbitrary data.
//!
//! This module implements an encoding scheme that can encode a size value using a variable
//! number of bytes based on the magnitude of the size, followed by arbitrary data bytes.
//! The size encoding uses a prefix to indicate how many bytes are used to represent the size:
//!
//! # Size Encoding Format
//!
//! The size value is encoded using one of three formats, chosen based on the value's magnitude:
//!
//! * 6-bit (1 byte total):  `00xxxxxx`
//!   - First 2 bits are `00`
//!   - Next 6 bits contain size value
//!   - Can encode sizes 0-63
//!
//! * 14-bit (2 bytes total): `01xxxxxx yyyyyyyy`
//!   - First 2 bits are `01`
//!   - Next 14 bits contain size value in big-endian order
//!   - Can encode sizes 64-16383
//!
//! * 32-bit (5 bytes total): `10000000 xxxxxxxx xxxxxxxx xxxxxxxx xxxxxxxx`
//!   - First 2 bits are `10`
//!   - Next 4 bytes contain size value in big-endian order
//!   - Can encode sizes 16384-4294967295
//!
//! After the size encoding, the arbitrary data bytes follow immediately.
//!
//! # Examples
//!
//! Basic usage:
//! ```rust
//! # fn main() -> Option<()> {
//! let data = "Hello".as_bytes();
//! let size = 42;
//!
//! // Encode size 42 (fits in 6 bits) followed by "Hello"
//! let encoded = size_encode(size, data)?;
//!
//! // First byte should be 00101010 (0b00 prefix + 42)
//! assert_eq!(encoded[0], 0b00101010);
//!
//! // Remaining bytes should be "Hello"
//! assert_eq!(&encoded[1..], b"Hello");
//! # Some(())
//! # }
//! ```
//!
//! The benefit is that size_encode allows you to encode any size value independently of the data length. This is useful when:
//!
//! 1. You're sending a header for a larger message:
//! ```rust,text
//! // First message part
//! let header = size_encode(1000, b"start").unwrap();
//! send(header);  // Sends: size=1000 + data="start"
//! // Later messages
//! send(b"more data");  // Just data
//! send(b"final part"); // Just data
//! ```
//!
//! It's primarily about communication/protocol rather than efficiency.\
use crate::{from_to, make_smart_pointer, services::statefuls::routers::cache_actor::CacheDb};
use anyhow::Result;
use key_value_storage_extractor::KeyValue;
use std::collections::HashMap;
use std::ops::RangeInclusive;
mod database_extractor;
mod key_value_storage_extractor;
pub mod size_encoding;

pub struct RdbFile {
    header: String,
    metadata: HashMap<String, String>,
    database: Vec<CacheDb>,
}

#[derive(Debug, PartialEq)]
pub struct DecodedData {
    // length of the data in bytes (including the size encoding)
    pub data: String,
}
// TODO rename it
#[derive(Default)]
pub struct BytesHandler(Vec<u8>);

impl BytesHandler {
    fn remove_identifier(&mut self) {
        self.remove(0);
    }

    fn try_when_0xFE(&mut self) -> Result<usize> {
        self.remove_identifier();
        self.try_size_decode()
    }
    fn try_when_0xFB(&mut self) -> Result<(usize, usize)> {
        self.remove_identifier();

        Ok((self.try_size_decode()?, self.try_size_decode()?))
    }
    fn when_0xFF(&mut self) -> Option<Vec<u8>> {
        self.remove_identifier();
        let checksum = extract_range(self, 0..=7).map(|f: [u8; 8]| f.to_vec());
        self.drain(..8);
        checksum
    }

    fn try_when_0xFC(&mut self) -> Result<u64> {
        self.remove_identifier();
        self.try_extract_expiry_time_in_milliseconds()
    }
    fn try_when_0xFD(&mut self) -> Result<u64> {
        self.remove_identifier();
        self.try_extract_expiry_time_in_seconds()
    }

    fn try_when_0x00(&mut self) -> Result<(String, String)> {
        self.remove_identifier();
        let key_data = self
            .string_decode()
            .ok_or(anyhow::anyhow!("key decode fail"))?;

        let value_data = self
            .string_decode()
            .ok_or(anyhow::anyhow!("value decode fail"))?;

        Ok((key_data.data, value_data.data))
    }

    fn try_extract_expiry_time_in_seconds(&mut self) -> Result<u64> {
        let range = 0..=3;
        let result = u32::from_le_bytes(
            extract_range(self, range.clone())
                .ok_or(anyhow::anyhow!("Failed to extract expiry time in seconds"))?,
        );
        self.drain(range);

        Ok(result as u64)
    }

    fn try_extract_expiry_time_in_milliseconds(&mut self) -> Result<u64> {
        let range = 0..=7;
        let result = u64::from_le_bytes(extract_range(self, range.clone()).ok_or(
            anyhow::anyhow!("Failed to extract expiry time in milliseconds"),
        )?);
        self.drain(range);
        Ok(result)
    }

    fn try_size_decode(&mut self) -> Result<usize> {
        self.size_decode()
            .ok_or(anyhow::anyhow!("size decode fail"))
    }
    // Decode a size-encoded value based on the first two bits and return the decoded value as a string.
    pub fn string_decode(&mut self) -> Option<DecodedData> {
        // Ensure we have at least one byte to read.
        if self.is_empty() {
            return None;
        }

        if let Some(size) = self.size_decode() {
            if size > self.len() {
                return None;
            }
            let data = String::from_utf8(self.drain(0..size).collect()).unwrap();
            Some(DecodedData { data })
        } else {
            self.integer_decode()
        }
    }

    pub fn size_decode(&mut self) -> Option<usize> {
        if let Some(first_byte) = self.get(0) {
            match first_byte >> 6 {
                0b00 => {
                    let size = (first_byte & 0x3F) as usize;
                    self.drain(0..1);
                    Some(size)
                }
                0b01 => {
                    if self.len() < 2 {
                        return None;
                    }
                    let size = (((first_byte & 0x3F) as usize) << 8) | (self[1] as usize);
                    self.drain(0..2);
                    Some(size)
                }
                0b10 => {
                    if self.len() < 5 {
                        return None;
                    }
                    let size = ((self[1] as usize) << 24)
                        | ((self[2] as usize) << 16)
                        | ((self[3] as usize) << 8)
                        | (self[4] as usize);
                    self.drain(0..5);
                    Some(size)
                }
                _ => None,
            }
        } else {
            None
        }
    }

    fn integer_decode(&mut self) -> Option<DecodedData> {
        if let Some(first_byte) = self.get(0) {
            match first_byte {
                // 0b11000000: 8-bit integer
                0xC0 => {
                    let value = u8::from_le_bytes([self[1]]).to_string();
                    self.drain(0..2);
                    return Some(DecodedData { data: value });
                }
                0xC1 => {
                    if self.len() == 3 {
                        let value = u16::from_le_bytes(extract_range(self, 1..=2)?).to_string();
                        self.drain(0..3);
                        return Some(DecodedData { data: value });
                    }
                }
                0xC2 => {
                    if self.len() == 5 {
                        let value = u32::from_le_bytes(extract_range(self, 1..=4)?);
                        self.drain(0..5);
                        return Some(DecodedData {
                            data: value.to_string(),
                        });
                    }
                }
                _ => return None,
            }
        }
        None
    }
}
make_smart_pointer!(BytesHandler, Vec<u8>);
from_to!(Vec<u8>, BytesHandler);

// Safe conversion from a slice to an array of a specific size.
fn extract_range<const N: usize>(encoded: &[u8], range: RangeInclusive<usize>) -> Option<[u8; N]> {
    TryInto::<[u8; N]>::try_into(encoded.get(range)?).ok()
}

#[test]
fn test_decoding() {
    // "Hello, World!"
    let mut example1: BytesHandler = vec![
        0x0D, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x2C, 0x20, 0x57, 0x6F, 0x72, 0x6C, 0x64, 0x21,
    ]
    .into();

    // "Test", with size 10 (although more bytes needed)
    let mut example2: BytesHandler = vec![0x42, 0x0A, 0x54, 0x65, 0x73, 0x74].into();

    assert!(example1.string_decode().is_some());
    assert!(example2.string_decode().is_none()); // due to insufficient bytes
}

#[test]
fn test_decode_multiple_strings() {
    // "abc" and "def"
    let mut encoded: BytesHandler = vec![0x03, 0x61, 0x62, 0x63, 0x03, 0x64, 0x65, 0x66].into();
    let decoded = encoded.string_decode().unwrap();
    assert_eq!(
        decoded,
        DecodedData {
            data: "abc".to_string()
        }
    );
    let decoded = encoded.string_decode().unwrap();
    assert_eq!(
        decoded,
        DecodedData {
            data: "def".to_string()
        }
    );
}
