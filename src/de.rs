//! Deserialization.

#[cfg(feature = "alloc")]
use alloc::format;
use cid::serde::CID_SERDE_PRIVATE_IDENTIFIER;
use core::convert::TryFrom;
use core::f32;
use core::marker::PhantomData;
use core::result;
use core::str;
use half::f16;
use serde::{de, forward_to_deserialize_any};
#[cfg(feature = "std")]
use std::io;

use crate::error::{Error, ErrorCode, Result};
#[cfg(not(feature = "unsealed_read_write"))]
use crate::read::EitherLifetime;
#[cfg(feature = "unsealed_read_write")]
pub use crate::read::EitherLifetime;
#[cfg(feature = "std")]
pub use crate::read::IoRead;
use crate::read::Offset;
#[cfg(any(feature = "std", feature = "alloc"))]
pub use crate::read::SliceRead;
pub use crate::read::{MutSliceRead, Read, SliceReadFixed};
use crate::{CBOR_TAGS_CID, CBOR_TAGS_MAJOR_TYPE_AND_CID};

/// Decodes a value from CBOR data in a slice.
///
/// # Examples
///
/// Deserialize a `String`
///
/// ```
/// # use serde_ipld_dagcbor::de;
/// let v: Vec<u8> = vec![0x66, 0x66, 0x6f, 0x6f, 0x62, 0x61, 0x72];
/// let value: String = de::from_slice(&v[..]).unwrap();
/// assert_eq!(value, "foobar");
/// ```
///
/// Deserialize a borrowed string with zero copies.
///
/// ```
/// # use serde_ipld_dagcbor::de;
/// let v: Vec<u8> = vec![0x66, 0x66, 0x6f, 0x6f, 0x62, 0x61, 0x72];
/// let value: &str = de::from_slice(&v[..]).unwrap();
/// assert_eq!(value, "foobar");
/// ```
#[cfg(any(feature = "std", feature = "alloc"))]
pub fn from_slice<'a, T>(slice: &'a [u8]) -> Result<T>
where
    T: de::Deserialize<'a>,
{
    let mut deserializer = Deserializer::from_slice(slice);
    let value = de::Deserialize::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

// When the "std" feature is enabled there should be little to no need to ever use this function,
// as `from_slice` covers all use cases (at the expense of being less efficient).
/// Decode a value from CBOR data in a mutable slice.
///
/// This can be used in analogy to `from_slice`. Unlike `from_slice`, this will use the slice's
/// mutability to rearrange data in it in order to resolve indefinite byte or text strings without
/// resorting to allocations.
pub fn from_mut_slice<'a, T>(slice: &'a mut [u8]) -> Result<T>
where
    T: de::Deserialize<'a>,
{
    let mut deserializer = Deserializer::from_mut_slice(slice);
    let value = de::Deserialize::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

// When the "std" feature is enabled there should be little to no need to ever use this function,
// as `from_slice` covers all use cases and is much more reliable (at the expense of being less
// efficient).
/// Decode a value from CBOR data using a scratch buffer.
///
/// Users should generally prefer to use `from_slice` or `from_mut_slice` over this function,
/// as decoding may fail when the scratch buffer turns out to be too small.
///
/// A realistic use case for this method would be decoding in a `no_std` environment from an
/// immutable slice that is too large to copy.
pub fn from_slice_with_scratch<'a, 'b, T>(slice: &'a [u8], scratch: &'b mut [u8]) -> Result<T>
where
    T: de::Deserialize<'a>,
{
    let mut deserializer = Deserializer::from_slice_with_scratch(slice, scratch);
    let value = de::Deserialize::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

/// Decodes a value from CBOR data in a reader.
///
/// # Examples
///
/// Deserialize a `String`
///
/// ```
/// # use serde_ipld_dagcbor::de;
/// let v: Vec<u8> = vec![0x66, 0x66, 0x6f, 0x6f, 0x62, 0x61, 0x72];
/// let value: String = de::from_reader(&v[..]).unwrap();
/// assert_eq!(value, "foobar");
/// ```
///
/// Note that `from_reader` cannot borrow data:
///
/// ```compile_fail
/// # use serde_ipld_dagcbor::de;
/// let v: Vec<u8> = vec![0x66, 0x66, 0x6f, 0x6f, 0x62, 0x61, 0x72];
/// let value: &str = de::from_reader(&v[..]).unwrap();
/// assert_eq!(value, "foobar");
/// ```
#[cfg(feature = "std")]
pub fn from_reader<T, R>(reader: R) -> Result<T>
where
    T: de::DeserializeOwned,
    R: io::Read,
{
    let mut deserializer = Deserializer::from_reader(reader);
    let value = de::Deserialize::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

/// A Serde `Deserialize`r of CBOR data.
#[derive(Debug)]
pub struct Deserializer<R> {
    read: R,
    remaining_depth: u8,
    accept_named: bool,
    accept_packed: bool,
    accept_standard_enums: bool,
    accept_legacy_enums: bool,
}

#[cfg(feature = "std")]
impl<R> Deserializer<IoRead<R>>
where
    R: io::Read,
{
    /// Constructs a `Deserializer` which reads from a `Read`er.
    pub fn from_reader(reader: R) -> Deserializer<IoRead<R>> {
        Deserializer::new(IoRead::new(reader))
    }
}

#[cfg(any(feature = "std", feature = "alloc"))]
impl<'a> Deserializer<SliceRead<'a>> {
    /// Constructs a `Deserializer` which reads from a slice.
    ///
    /// Borrowed strings and byte slices will be provided when possible.
    pub fn from_slice(bytes: &'a [u8]) -> Deserializer<SliceRead<'a>> {
        Deserializer::new(SliceRead::new(bytes))
    }
}

impl<'a> Deserializer<MutSliceRead<'a>> {
    /// Constructs a `Deserializer` which reads from a mutable slice that doubles as its own
    /// scratch buffer.
    ///
    /// Borrowed strings and byte slices will be provided even for indefinite strings.
    pub fn from_mut_slice(bytes: &'a mut [u8]) -> Deserializer<MutSliceRead<'a>> {
        Deserializer::new(MutSliceRead::new(bytes))
    }
}

impl<'a, 'b> Deserializer<SliceReadFixed<'a, 'b>> {
    #[doc(hidden)]
    pub fn from_slice_with_scratch(
        bytes: &'a [u8],
        scratch: &'b mut [u8],
    ) -> Deserializer<SliceReadFixed<'a, 'b>> {
        Deserializer::new(SliceReadFixed::new(bytes, scratch))
    }
}

impl<'de, R> Deserializer<R>
where
    R: Read<'de>,
{
    /// Constructs a `Deserializer` from one of the possible serde_ipld_dagcbor input sources.
    ///
    /// `from_slice` and `from_reader` should normally be used instead of this method.
    pub fn new(read: R) -> Self {
        Deserializer {
            read,
            remaining_depth: 128,
            accept_named: true,
            accept_packed: true,
            accept_standard_enums: true,
            accept_legacy_enums: true,
        }
    }

    /// Don't accept named variants and fields.
    pub fn disable_named_format(mut self) -> Self {
        self.accept_named = false;
        self
    }

    /// Don't accept numbered variants and fields.
    pub fn disable_packed_format(mut self) -> Self {
        self.accept_packed = false;
        self
    }

    /// Don't accept the new enum format used by `serde_ipld_dagcbor` versions >= v0.10.
    pub fn disable_standard_enums(mut self) -> Self {
        self.accept_standard_enums = false;
        self
    }

    /// Don't accept the old enum format used by `serde_ipld_dagcbor` versions <= v0.9.
    pub fn disable_legacy_enums(mut self) -> Self {
        self.accept_legacy_enums = false;
        self
    }

    /// This method should be called after a value has been deserialized to ensure there is no
    /// trailing data in the input source.
    pub fn end(&mut self) -> Result<()> {
        match self.next()? {
            Some(_) => Err(self.error(ErrorCode::TrailingData)),
            None => Ok(()),
        }
    }

    /// Turn a CBOR deserializer into an iterator over values of type T.
    #[allow(clippy::should_implement_trait)] // Trait doesn't allow unconstrained T.
    pub fn into_iter<T>(self) -> StreamDeserializer<'de, R, T>
    where
        T: de::Deserialize<'de>,
    {
        StreamDeserializer {
            de: self,
            output: PhantomData,
            lifetime: PhantomData,
        }
    }

    fn next(&mut self) -> Result<Option<u8>> {
        self.read.next()
    }

    fn peek(&mut self) -> Result<Option<u8>> {
        self.read.peek()
    }

    fn consume(&mut self) {
        self.read.discard();
    }

    fn error(&self, reason: ErrorCode) -> Error {
        let offset = self.read.offset();
        Error::syntax(reason, offset)
    }

    fn parse_u8(&mut self) -> Result<u8> {
        match self.next()? {
            Some(byte) => Ok(byte),
            None => Err(self.error(ErrorCode::EofWhileParsingValue)),
        }
    }

    fn parse_u16(&mut self) -> Result<u16> {
        let mut buf = [0; 2];
        self.read
            .read_into(&mut buf)
            .map(|()| u16::from_be_bytes(buf))
    }

    fn parse_u32(&mut self) -> Result<u32> {
        let mut buf = [0; 4];
        self.read
            .read_into(&mut buf)
            .map(|()| u32::from_be_bytes(buf))
    }

    fn parse_u64(&mut self) -> Result<u64> {
        let mut buf = [0; 8];
        self.read
            .read_into(&mut buf)
            .map(|()| u64::from_be_bytes(buf))
    }

    fn parse_bytes<V>(&mut self, len: usize, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        match self.read.read(len)? {
            EitherLifetime::Long(buf) => visitor.visit_borrowed_bytes(buf),
            EitherLifetime::Short(buf) => visitor.visit_bytes(buf),
        }
    }

    fn parse_indefinite_bytes<V>(&mut self, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        self.read.clear_buffer();
        loop {
            let byte = self.parse_u8()?;
            let len = match byte {
                0x40..=0x57 => byte as usize - 0x40,
                0x58 => self.parse_u8()? as usize,
                0x59 => self.parse_u16()? as usize,
                0x5a => self.parse_u32()? as usize,
                0x5b => {
                    let len = self.parse_u64()?;
                    if len > usize::max_value() as u64 {
                        return Err(self.error(ErrorCode::LengthOutOfRange));
                    }
                    len as usize
                }
                0xff => break,
                _ => return Err(self.error(ErrorCode::UnexpectedCode)),
            };

            self.read.read_to_buffer(len)?;
        }

        match self.read.take_buffer() {
            EitherLifetime::Long(buf) => visitor.visit_borrowed_bytes(buf),
            EitherLifetime::Short(buf) => visitor.visit_bytes(buf),
        }
    }

    fn convert_str(buf: &[u8], buf_end_offset: u64) -> Result<&str> {
        #[cfg(not(feature = "_do_not_use_its_unsafe_and_invalid_cbor"))]
        match str::from_utf8(buf) {
            Ok(s) => Ok(s),
            Err(e) => {
                let shift = buf.len() - e.valid_up_to();
                let offset = buf_end_offset - shift as u64;
                Err(Error::syntax(ErrorCode::InvalidUtf8, offset))
            }
        }

        // Don't use this. This can lead to random panics and invalid CBOR.
        #[cfg(feature = "_do_not_use_its_unsafe_and_invalid_cbor")]
        Ok(unsafe { str::from_utf8_unchecked(buf) })
    }

    fn parse_str<V>(&mut self, len: usize, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        if let Some(offset) = self.read.offset().checked_add(len as u64) {
            match self.read.read(len)? {
                EitherLifetime::Long(buf) => {
                    let s = Self::convert_str(buf, offset)?;
                    visitor.visit_borrowed_str(s)
                }
                EitherLifetime::Short(buf) => {
                    let s = Self::convert_str(buf, offset)?;
                    visitor.visit_str(s)
                }
            }
        } else {
            // An overflow would have occured.
            Err(Error::syntax(
                ErrorCode::LengthOutOfRange,
                self.read.offset(),
            ))
        }
    }

    fn parse_indefinite_str<V>(&mut self, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        self.read.clear_buffer();
        loop {
            let byte = self.parse_u8()?;
            let len = match byte {
                0x60..=0x77 => byte as usize - 0x60,
                0x78 => self.parse_u8()? as usize,
                0x79 => self.parse_u16()? as usize,
                0x7a => self.parse_u32()? as usize,
                0x7b => {
                    let len = self.parse_u64()?;
                    if len > usize::max_value() as u64 {
                        return Err(self.error(ErrorCode::LengthOutOfRange));
                    }
                    len as usize
                }
                0xff => break,
                _ => return Err(self.error(ErrorCode::UnexpectedCode)),
            };

            self.read.read_to_buffer(len)?;
        }

        let offset = self.read.offset();
        match self.read.take_buffer() {
            EitherLifetime::Long(buf) => {
                let s = Self::convert_str(buf, offset)?;
                visitor.visit_borrowed_str(s)
            }
            EitherLifetime::Short(buf) => {
                let s = Self::convert_str(buf, offset)?;
                visitor.visit_str(s)
            }
        }
    }

    fn recursion_checked<F, T>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Deserializer<R>) -> Result<T>,
    {
        self.remaining_depth -= 1;
        if self.remaining_depth == 0 {
            return Err(self.error(ErrorCode::RecursionLimitExceeded));
        }
        let r = f(self);
        self.remaining_depth += 1;
        r
    }

    fn parse_array<V>(&mut self, mut len: usize, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        self.recursion_checked(|de| {
            let value = visitor.visit_seq(SeqAccess { de, len: &mut len })?;

            if len != 0 {
                Err(de.error(ErrorCode::TrailingData))
            } else {
                Ok(value)
            }
        })
    }

    fn parse_indefinite_array<V>(&mut self, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        self.recursion_checked(|de| {
            let value = visitor.visit_seq(IndefiniteSeqAccess { de })?;
            match de.next()? {
                Some(0xff) => Ok(value),
                Some(_) => Err(de.error(ErrorCode::TrailingData)),
                None => Err(de.error(ErrorCode::EofWhileParsingArray)),
            }
        })
    }

    fn parse_map<V>(&mut self, mut len: usize, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        let accept_packed = self.accept_packed;
        let accept_named = self.accept_named;
        self.recursion_checked(|de| {
            let value = visitor.visit_map(MapAccess {
                de,
                len: &mut len,
                accept_named,
                accept_packed,
            })?;

            if len != 0 {
                Err(de.error(ErrorCode::TrailingData))
            } else {
                Ok(value)
            }
        })
    }

    fn parse_indefinite_map<V>(&mut self, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        let accept_named = self.accept_named;
        let accept_packed = self.accept_packed;
        self.recursion_checked(|de| {
            let value = visitor.visit_map(IndefiniteMapAccess {
                de,
                accept_packed,
                accept_named,
            })?;
            match de.next()? {
                Some(0xff) => Ok(value),
                Some(_) => Err(de.error(ErrorCode::TrailingData)),
                None => Err(de.error(ErrorCode::EofWhileParsingMap)),
            }
        })
    }

    fn parse_enum<V>(&mut self, mut len: usize, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        self.recursion_checked(|de| {
            let value = visitor.visit_enum(VariantAccess {
                seq: SeqAccess { de, len: &mut len },
            })?;

            if len != 0 {
                Err(de.error(ErrorCode::TrailingData))
            } else {
                Ok(value)
            }
        })
    }

    fn parse_enum_map<V>(&mut self, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        let accept_named = self.accept_named;
        let accept_packed = self.accept_packed;
        self.recursion_checked(|de| {
            let mut len = 1;
            let value = visitor.visit_enum(VariantAccessMap {
                map: MapAccess {
                    de,
                    len: &mut len,
                    accept_packed,
                    accept_named,
                },
            })?;

            if len != 0 {
                Err(de.error(ErrorCode::TrailingData))
            } else {
                Ok(value)
            }
        })
    }

    fn parse_indefinite_enum<V>(&mut self, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        self.recursion_checked(|de| {
            let value = visitor.visit_enum(VariantAccess {
                seq: IndefiniteSeqAccess { de },
            })?;
            match de.next()? {
                Some(0xff) => Ok(value),
                Some(_) => Err(de.error(ErrorCode::TrailingData)),
                None => Err(de.error(ErrorCode::EofWhileParsingArray)),
            }
        })
    }

    fn parse_f16(&mut self) -> Result<f32> {
        Ok(f32::from(f16::from_bits(self.parse_u16()?)))
    }

    fn parse_f32(&mut self) -> Result<f32> {
        self.parse_u32().map(f32::from_bits)
    }

    fn parse_f64(&mut self) -> Result<f64> {
        self.parse_u64().map(f64::from_bits)
    }

    fn parse_cid<V>(&mut self, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        self.recursion_checked(|de| visitor.visit_newtype_struct(&mut CidDeserializer(de)))
    }

    // Don't warn about the `unreachable!` in case
    // exhaustive integer pattern matching is enabled.
    #[allow(unreachable_patterns)]
    fn parse_value<V>(&mut self, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        let byte = self.parse_u8()?;
        match byte {
            // Major type 0: an unsigned integer
            0x00..=0x17 => visitor.visit_u8(byte),
            0x18 => {
                let value = self.parse_u8()?;
                visitor.visit_u8(value)
            }
            0x19 => {
                let value = self.parse_u16()?;
                visitor.visit_u16(value)
            }
            0x1a => {
                let value = self.parse_u32()?;
                visitor.visit_u32(value)
            }
            0x1b => {
                let value = self.parse_u64()?;
                visitor.visit_u64(value)
            }
            0x1c..=0x1f => Err(self.error(ErrorCode::UnassignedCode)),

            // Major type 1: a negative integer
            0x20..=0x37 => visitor.visit_i8(-1 - (byte - 0x20) as i8),
            0x38 => {
                let value = self.parse_u8()?;
                visitor.visit_i16(-1 - i16::from(value))
            }
            0x39 => {
                let value = self.parse_u16()?;
                visitor.visit_i32(-1 - i32::from(value))
            }
            0x3a => {
                let value = self.parse_u32()?;
                visitor.visit_i64(-1 - i64::from(value))
            }
            0x3b => {
                let value = self.parse_u64()?;
                if value > i64::max_value() as u64 {
                    return visitor.visit_i128(-1 - i128::from(value));
                }
                visitor.visit_i64(-1 - value as i64)
            }
            0x3c..=0x3f => Err(self.error(ErrorCode::UnassignedCode)),

            // Major type 2: a byte string
            0x40..=0x57 => self.parse_bytes(byte as usize - 0x40, visitor),
            0x58 => {
                let len = self.parse_u8()?;
                self.parse_bytes(len as usize, visitor)
            }
            0x59 => {
                let len = self.parse_u16()?;
                self.parse_bytes(len as usize, visitor)
            }
            0x5a => {
                let len = self.parse_u32()?;
                self.parse_bytes(len as usize, visitor)
            }
            0x5b => {
                let len = self.parse_u64()?;
                if len > usize::max_value() as u64 {
                    return Err(self.error(ErrorCode::LengthOutOfRange));
                }
                self.parse_bytes(len as usize, visitor)
            }
            0x5c..=0x5e => Err(self.error(ErrorCode::UnassignedCode)),
            0x5f => self.parse_indefinite_bytes(visitor),

            // Major type 3: a text string
            0x60..=0x77 => self.parse_str(byte as usize - 0x60, visitor),
            0x78 => {
                let len = self.parse_u8()?;
                self.parse_str(len as usize, visitor)
            }
            0x79 => {
                let len = self.parse_u16()?;
                self.parse_str(len as usize, visitor)
            }
            0x7a => {
                let len = self.parse_u32()?;
                self.parse_str(len as usize, visitor)
            }
            0x7b => {
                let len = self.parse_u64()?;
                if len > usize::max_value() as u64 {
                    return Err(self.error(ErrorCode::LengthOutOfRange));
                }
                self.parse_str(len as usize, visitor)
            }
            0x7c..=0x7e => Err(self.error(ErrorCode::UnassignedCode)),
            0x7f => self.parse_indefinite_str(visitor),

            // Major type 4: an array of data items
            0x80..=0x97 => self.parse_array(byte as usize - 0x80, visitor),
            0x98 => {
                let len = self.parse_u8()?;
                self.parse_array(len as usize, visitor)
            }
            0x99 => {
                let len = self.parse_u16()?;
                self.parse_array(len as usize, visitor)
            }
            0x9a => {
                let len = self.parse_u32()?;
                self.parse_array(len as usize, visitor)
            }
            0x9b => {
                let len = self.parse_u64()?;
                if len > usize::max_value() as u64 {
                    return Err(self.error(ErrorCode::LengthOutOfRange));
                }
                self.parse_array(len as usize, visitor)
            }
            0x9c..=0x9e => Err(self.error(ErrorCode::UnassignedCode)),
            0x9f => self.parse_indefinite_array(visitor),

            // Major type 5: a map of pairs of data items
            0xa0..=0xb7 => self.parse_map(byte as usize - 0xa0, visitor),
            0xb8 => {
                let len = self.parse_u8()?;
                self.parse_map(len as usize, visitor)
            }
            0xb9 => {
                let len = self.parse_u16()?;
                self.parse_map(len as usize, visitor)
            }
            0xba => {
                let len = self.parse_u32()?;
                self.parse_map(len as usize, visitor)
            }
            0xbb => {
                let len = self.parse_u64()?;
                if len > usize::max_value() as u64 {
                    return Err(self.error(ErrorCode::LengthOutOfRange));
                }
                self.parse_map(len as usize, visitor)
            }
            0xbc..=0xbe => Err(self.error(ErrorCode::UnassignedCode)),
            0xbf => self.parse_indefinite_map(visitor),

            // Major type 6: optional semantic tagging of other major types
            // Only tag 42 is supported, hence we refuse parsing any other tags here.
            0xc0..=0xd7 => Err(self.error(ErrorCode::UnexpectedCode)),
            0xd8 => {
                if self.parse_u8()? == CBOR_TAGS_CID {
                    self.parse_cid(visitor)
                } else {
                    Err(self.error(ErrorCode::UnexpectedCode))
                }
            }
            0xd9..=0xdb => Err(self.error(ErrorCode::UnexpectedCode)),
            0xdc..=0xdf => Err(self.error(ErrorCode::UnassignedCode)),

            // Major type 7: floating-point numbers and other simple data types that need no content
            0xe0..=0xf3 => Err(self.error(ErrorCode::UnassignedCode)),
            0xf4 => visitor.visit_bool(false),
            0xf5 => visitor.visit_bool(true),
            0xf6 => visitor.visit_none(),
            // DAG-CBOR doesn't support `undefined`
            0xf7 => Err(self.error(ErrorCode::UnexpectedCode)),
            0xf8 => Err(self.error(ErrorCode::UnassignedCode)),
            0xf9 => {
                let value = self.parse_f16()?;
                visitor.visit_f32(value)
            }
            0xfa => {
                let value = self.parse_f32()?;
                visitor.visit_f32(value)
            }
            0xfb => {
                let value = self.parse_f64()?;
                visitor.visit_f64(value)
            }
            0xfc..=0xfe => Err(self.error(ErrorCode::UnassignedCode)),
            0xff => Err(self.error(ErrorCode::UnexpectedCode)),

            _ => unreachable!(),
        }
    }
}

impl<'de, 'a, R> de::Deserializer<'de> for &'a mut Deserializer<R>
where
    R: Read<'de>,
{
    type Error = Error;

    #[inline]
    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        self.parse_value(visitor)
    }

    #[inline]
    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        match self.peek()? {
            Some(0xf6) => {
                self.consume();
                visitor.visit_none()
            }
            _ => visitor.visit_some(self),
        }
    }

    #[inline]
    fn deserialize_newtype_struct<V>(self, name: &str, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        if name == CID_SERDE_PRIVATE_IDENTIFIER {
            // It's only valid if there is really an encoded CID.
            match self.parse_u16()? {
                CBOR_TAGS_MAJOR_TYPE_AND_CID => self.parse_cid(visitor),
                _ => Err(self.error(ErrorCode::UnexpectedCode)),
            }
        } else {
            visitor.visit_newtype_struct(self)
        }
    }

    // Unit variants are encoded as just the variant identifier.
    // Tuple variants are encoded as an array of the variant identifier followed by the fields.
    // Struct variants are encoded as an array of the variant identifier followed by the struct.
    #[inline]
    fn deserialize_enum<V>(
        self,
        _name: &str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        match self.peek()? {
            Some(byte @ 0x80..=0x9f) => {
                if !self.accept_legacy_enums {
                    return Err(self.error(ErrorCode::WrongEnumFormat));
                }
                self.consume();
                match byte {
                    0x80..=0x97 => self.parse_enum(byte as usize - 0x80, visitor),
                    0x98 => {
                        let len = self.parse_u8()?;
                        self.parse_enum(len as usize, visitor)
                    }
                    0x99 => {
                        let len = self.parse_u16()?;
                        self.parse_enum(len as usize, visitor)
                    }
                    0x9a => {
                        let len = self.parse_u32()?;
                        self.parse_enum(len as usize, visitor)
                    }
                    0x9b => {
                        let len = self.parse_u64()?;
                        if len > usize::max_value() as u64 {
                            return Err(self.error(ErrorCode::LengthOutOfRange));
                        }
                        self.parse_enum(len as usize, visitor)
                    }
                    0x9c..=0x9e => Err(self.error(ErrorCode::UnassignedCode)),
                    0x9f => self.parse_indefinite_enum(visitor),

                    _ => unreachable!(),
                }
            }
            Some(0xa1) => {
                if !self.accept_standard_enums {
                    return Err(self.error(ErrorCode::WrongEnumFormat));
                }
                self.consume();
                self.parse_enum_map(visitor)
            }
            None => Err(self.error(ErrorCode::EofWhileParsingValue)),
            _ => {
                if !self.accept_standard_enums && !self.accept_legacy_enums {
                    return Err(self.error(ErrorCode::WrongEnumFormat));
                }
                visitor.visit_enum(UnitVariantAccess { de: self })
            }
        }
    }

    #[inline]
    fn is_human_readable(&self) -> bool {
        false
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string unit
        unit_struct seq tuple tuple_struct map struct identifier ignored_any
        bytes byte_buf
    }
}

impl<R> Deserializer<R>
where
    R: Offset,
{
    /// Return the current offset in the reader
    #[inline]
    pub fn byte_offset(&self) -> usize {
        self.read.byte_offset()
    }
}

trait MakeError {
    fn error(&self, code: ErrorCode) -> Error;
}

struct SeqAccess<'a, R> {
    de: &'a mut Deserializer<R>,
    len: &'a mut usize,
}

impl<'de, 'a, R> de::SeqAccess<'de> for SeqAccess<'a, R>
where
    R: Read<'de>,
{
    type Error = Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>>
    where
        T: de::DeserializeSeed<'de>,
    {
        if *self.len == 0 {
            return Ok(None);
        }
        *self.len -= 1;

        let value = seed.deserialize(&mut *self.de)?;
        Ok(Some(value))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(*self.len)
    }
}

impl<'de, 'a, R> MakeError for SeqAccess<'a, R>
where
    R: Read<'de>,
{
    fn error(&self, code: ErrorCode) -> Error {
        self.de.error(code)
    }
}

struct IndefiniteSeqAccess<'a, R> {
    de: &'a mut Deserializer<R>,
}

impl<'de, 'a, R> de::SeqAccess<'de> for IndefiniteSeqAccess<'a, R>
where
    R: Read<'de>,
{
    type Error = Error;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>>
    where
        T: de::DeserializeSeed<'de>,
    {
        match self.de.peek()? {
            Some(0xff) => return Ok(None),
            Some(_) => {}
            None => return Err(self.de.error(ErrorCode::EofWhileParsingArray)),
        }

        let value = seed.deserialize(&mut *self.de)?;
        Ok(Some(value))
    }
}

impl<'de, 'a, R> MakeError for IndefiniteSeqAccess<'a, R>
where
    R: Read<'de>,
{
    fn error(&self, code: ErrorCode) -> Error {
        self.de.error(code)
    }
}

struct MapAccess<'a, R> {
    de: &'a mut Deserializer<R>,
    len: &'a mut usize,
    accept_named: bool,
    accept_packed: bool,
}

impl<'de, 'a, R> de::MapAccess<'de> for MapAccess<'a, R>
where
    R: Read<'de>,
{
    type Error = Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
    where
        K: de::DeserializeSeed<'de>,
    {
        if *self.len == 0 {
            return Ok(None);
        }
        *self.len -= 1;

        match self.de.peek()? {
            Some(_byte @ 0x00..=0x1b) if !self.accept_packed => {
                return Err(self.de.error(ErrorCode::WrongStructFormat));
            }
            Some(_byte @ 0x60..=0x7f) if !self.accept_named => {
                return Err(self.de.error(ErrorCode::WrongStructFormat));
            }
            _ => {}
        };

        let value = seed.deserialize(&mut *self.de)?;
        Ok(Some(value))
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value>
    where
        V: de::DeserializeSeed<'de>,
    {
        seed.deserialize(&mut *self.de)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(*self.len)
    }
}

impl<'de, 'a, R> MakeError for MapAccess<'a, R>
where
    R: Read<'de>,
{
    fn error(&self, code: ErrorCode) -> Error {
        self.de.error(code)
    }
}

struct IndefiniteMapAccess<'a, R> {
    de: &'a mut Deserializer<R>,
    accept_packed: bool,
    accept_named: bool,
}

impl<'de, 'a, R> de::MapAccess<'de> for IndefiniteMapAccess<'a, R>
where
    R: Read<'de>,
{
    type Error = Error;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>>
    where
        K: de::DeserializeSeed<'de>,
    {
        match self.de.peek()? {
            Some(_byte @ 0x00..=0x1b) if !self.accept_packed => {
                return Err(self.de.error(ErrorCode::WrongStructFormat))
            }
            Some(_byte @ 0x60..=0x7f) if !self.accept_named => {
                return Err(self.de.error(ErrorCode::WrongStructFormat))
            }
            Some(0xff) => return Ok(None),
            Some(_) => {}
            None => return Err(self.de.error(ErrorCode::EofWhileParsingMap)),
        }

        let value = seed.deserialize(&mut *self.de)?;
        Ok(Some(value))
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value>
    where
        V: de::DeserializeSeed<'de>,
    {
        seed.deserialize(&mut *self.de)
    }
}

struct UnitVariantAccess<'a, R> {
    de: &'a mut Deserializer<R>,
}

impl<'de, 'a, R> de::EnumAccess<'de> for UnitVariantAccess<'a, R>
where
    R: Read<'de>,
{
    type Error = Error;
    type Variant = UnitVariantAccess<'a, R>;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, UnitVariantAccess<'a, R>)>
    where
        V: de::DeserializeSeed<'de>,
    {
        let variant = seed.deserialize(&mut *self.de)?;
        Ok((variant, self))
    }
}

impl<'de, 'a, R> de::VariantAccess<'de> for UnitVariantAccess<'a, R>
where
    R: Read<'de>,
{
    type Error = Error;

    fn unit_variant(self) -> Result<()> {
        Ok(())
    }

    fn newtype_variant_seed<T>(self, _seed: T) -> Result<T::Value>
    where
        T: de::DeserializeSeed<'de>,
    {
        Err(de::Error::invalid_type(
            de::Unexpected::UnitVariant,
            &"newtype variant",
        ))
    }

    fn tuple_variant<V>(self, _len: usize, _visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        Err(de::Error::invalid_type(
            de::Unexpected::UnitVariant,
            &"tuple variant",
        ))
    }

    fn struct_variant<V>(self, _fields: &'static [&'static str], _visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        Err(de::Error::invalid_type(
            de::Unexpected::UnitVariant,
            &"struct variant",
        ))
    }
}

struct VariantAccess<T> {
    seq: T,
}

impl<'de, T> de::EnumAccess<'de> for VariantAccess<T>
where
    T: de::SeqAccess<'de, Error = Error> + MakeError,
{
    type Error = Error;
    type Variant = VariantAccess<T>;

    fn variant_seed<V>(mut self, seed: V) -> Result<(V::Value, VariantAccess<T>)>
    where
        V: de::DeserializeSeed<'de>,
    {
        let variant = match self.seq.next_element_seed(seed) {
            Ok(Some(variant)) => variant,
            Ok(None) => return Err(self.seq.error(ErrorCode::ArrayTooShort)),
            Err(e) => return Err(e),
        };
        Ok((variant, self))
    }
}

impl<'de, T> de::VariantAccess<'de> for VariantAccess<T>
where
    T: de::SeqAccess<'de, Error = Error> + MakeError,
{
    type Error = Error;

    fn unit_variant(mut self) -> Result<()> {
        match self.seq.next_element() {
            Ok(Some(())) => Ok(()),
            Ok(None) => Err(self.seq.error(ErrorCode::ArrayTooLong)),
            Err(e) => Err(e),
        }
    }

    fn newtype_variant_seed<S>(mut self, seed: S) -> Result<S::Value>
    where
        S: de::DeserializeSeed<'de>,
    {
        match self.seq.next_element_seed(seed) {
            Ok(Some(variant)) => Ok(variant),
            Ok(None) => Err(self.seq.error(ErrorCode::ArrayTooShort)),
            Err(e) => Err(e),
        }
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_seq(self.seq)
    }

    fn struct_variant<V>(mut self, _fields: &'static [&'static str], visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        let seed = StructVariantSeed { visitor };
        match self.seq.next_element_seed(seed) {
            Ok(Some(variant)) => Ok(variant),
            Ok(None) => Err(self.seq.error(ErrorCode::ArrayTooShort)),
            Err(e) => Err(e),
        }
    }
}

struct StructVariantSeed<V> {
    visitor: V,
}

impl<'de, V> de::DeserializeSeed<'de> for StructVariantSeed<V>
where
    V: de::Visitor<'de>,
{
    type Value = V::Value;

    fn deserialize<D>(self, de: D) -> result::Result<V::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        de.deserialize_any(self.visitor)
    }
}

/// Iterator that deserializes a stream into multiple CBOR values.
///
/// A stream deserializer can be created from any CBOR deserializer using the
/// `Deserializer::into_iter` method.
///
/// ```
/// # extern crate serde_ipld_dagcbor;
/// use serde_ipld_dagcbor::de::Deserializer;
/// use libipld_core::ipld::Ipld;
///
/// # fn main() {
/// let data: Vec<u8> = vec![
///     0x01, 0x66, 0x66, 0x6f, 0x6f, 0x62, 0x61, 0x72,
/// ];
/// let mut it = Deserializer::from_slice(&data[..]).into_iter::<Ipld>();
/// assert_eq!(
///     Ipld::Integer(1),
///     it.next().unwrap().unwrap()
/// );
/// assert_eq!(
///     Ipld::String("foobar".to_string()),
///     it.next().unwrap().unwrap()
/// );
/// # }
/// ```
#[derive(Debug)]
pub struct StreamDeserializer<'de, R, T> {
    de: Deserializer<R>,
    output: PhantomData<T>,
    lifetime: PhantomData<&'de ()>,
}

impl<'de, R, T> StreamDeserializer<'de, R, T>
where
    R: Read<'de>,
    T: de::Deserialize<'de>,
{
    /// Create a new CBOR stream deserializer from one of the possible
    /// serde_ipld_dagcbor input sources.
    ///
    /// Typically it is more convenient to use one of these methods instead:
    ///
    /// * `Deserializer::from_slice(...).into_iter()`
    /// * `Deserializer::from_reader(...).into_iter()`
    pub fn new(read: R) -> StreamDeserializer<'de, R, T> {
        StreamDeserializer {
            de: Deserializer::new(read),
            output: PhantomData,
            lifetime: PhantomData,
        }
    }
}

impl<'de, R, T> StreamDeserializer<'de, R, T>
where
    R: Offset,
    T: de::Deserialize<'de>,
{
    /// Return the current offset in the reader
    #[inline]
    pub fn byte_offset(&self) -> usize {
        self.de.byte_offset()
    }
}

impl<'de, R, T> Iterator for StreamDeserializer<'de, R, T>
where
    R: Read<'de>,
    T: de::Deserialize<'de>,
{
    type Item = Result<T>;

    fn next(&mut self) -> Option<Result<T>> {
        match self.de.peek() {
            Ok(Some(_)) => Some(T::deserialize(&mut self.de)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

struct VariantAccessMap<T> {
    map: T,
}

impl<'de, T> de::EnumAccess<'de> for VariantAccessMap<T>
where
    T: de::MapAccess<'de, Error = Error> + MakeError,
{
    type Error = Error;
    type Variant = VariantAccessMap<T>;

    fn variant_seed<V>(mut self, seed: V) -> Result<(V::Value, VariantAccessMap<T>)>
    where
        V: de::DeserializeSeed<'de>,
    {
        let variant = match self.map.next_key_seed(seed) {
            Ok(Some(variant)) => variant,
            Ok(None) => return Err(self.map.error(ErrorCode::ArrayTooShort)),
            Err(e) => return Err(e),
        };
        Ok((variant, self))
    }
}

impl<'de, T> de::VariantAccess<'de> for VariantAccessMap<T>
where
    T: de::MapAccess<'de, Error = Error> + MakeError,
{
    type Error = Error;

    fn unit_variant(mut self) -> Result<()> {
        match self.map.next_value() {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn newtype_variant_seed<S>(mut self, seed: S) -> Result<S::Value>
    where
        S: de::DeserializeSeed<'de>,
    {
        self.map.next_value_seed(seed)
    }

    fn tuple_variant<V>(mut self, _len: usize, visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        let seed = StructVariantSeed { visitor };
        self.map.next_value_seed(seed)
    }

    fn struct_variant<V>(mut self, _fields: &'static [&'static str], visitor: V) -> Result<V::Value>
    where
        V: de::Visitor<'de>,
    {
        let seed = StructVariantSeed { visitor };
        self.map.next_value_seed(seed)
    }
}

/// Deserialize a DAG-CBOR encoded CID.
///
/// This is without the CBOR tag information. It is only the CBOR byte string identifier (major
/// type 2), the number of bytes, and a null byte prefixed CID.
///
/// The reason for not including the CBOR tag information is the [`Value`] implementation. That one
/// starts to parse the bytes, before we could interfere. If the data only includes a CID, we are
/// parsing over the tag to determine whether it is a CID or not and go from there.
struct CidDeserializer<'a, R>(&'a mut Deserializer<R>);

//impl<'de, 'a: 'de, R> de::Deserializer<'de> for &'a mut CidDeserializer<'a, R>
impl<'de, 'a, R> de::Deserializer<'de> for &'a mut CidDeserializer<'a, R>
where
    R: Read<'de>,
{
    type Error = Error;

    fn deserialize_any<V: de::Visitor<'de>>(self, _visitor: V) -> Result<V::Value> {
        Err(Error::message("Only bytes can be deserialized into a CID"))
    }

    #[inline]
    fn deserialize_bytes<V: de::Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        // Match on the major type, it must be a byte string (major type 2)
        let len = match self.0.parse_u8()? {
            // CIDs always have a `0x00` prefix, hence they cannot be zero sized.
            0x40 => return Err(self.0.error(ErrorCode::LengthOutOfRange)),
            byte @ 0x41..=0x57 => usize::try_from(byte - 0x40)
                .map_err(|_| self.0.error(ErrorCode::LengthOutOfRange))?,
            0x58 => {
                let len = self.0.parse_u8()?;
                usize::try_from(len).map_err(|_| self.0.error(ErrorCode::LengthOutOfRange))?
            }
            0x59 => {
                let len = self.0.parse_u16()?;
                usize::try_from(len).map_err(|_| self.0.error(ErrorCode::LengthOutOfRange))?
            }
            0x5a => {
                let len = self.0.parse_u32()?;
                usize::try_from(len).map_err(|_| self.0.error(ErrorCode::LengthOutOfRange))?
            }
            0x5b => {
                let len = self.0.parse_u64()?;
                usize::try_from(len).map_err(|_| self.0.error(ErrorCode::LengthOutOfRange))?
            }
            _ => return Err(self.0.error(ErrorCode::UnexpectedCode)),
        };

        match self.0.read.read(len)? {
            EitherLifetime::Long(buf) | EitherLifetime::Short(buf) => {
                // In DAG-CBOR the CID is prefixed with a null byte, strip that off.
                visitor.visit_bytes(&buf[1..])
            }
        }
    }

    fn deserialize_newtype_struct<V: de::Visitor<'de>>(
        self,
        name: &str,
        visitor: V,
    ) -> Result<V::Value> {
        if name == CID_SERDE_PRIVATE_IDENTIFIER {
            self.deserialize_bytes(visitor)
        } else {
            return Err(Error::message(format!(
                "This deserializer must not be called on newtype structs other than one named `{}`",
                CID_SERDE_PRIVATE_IDENTIFIER
            )));
        }
    }

    forward_to_deserialize_any! {
        bool byte_buf char enum f32 f64 i8 i16 i32 i64 identifier ignored_any map option seq str
        string struct tuple tuple_struct u8 u16 u32 u64 unit unit_struct
    }
}
