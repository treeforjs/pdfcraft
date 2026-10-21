//! Allocation-free admission before JSON escaping/serialization touches large strings.
use serde::Serialize;
use serde::ser::{self, Serializer};

#[derive(Debug)]
pub(crate) struct Error;
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("script message exceeds its byte, item or nesting budget")
    }
}
impl std::error::Error for Error {}
impl ser::Error for Error {
    fn custom<T: std::fmt::Display>(_: T) -> Self {
        Self
    }
}

pub(crate) struct Budget {
    remaining: usize,
    depth: usize,
}
impl Budget {
    pub fn new(remaining: usize) -> Self {
        Self { remaining, depth: 0 }
    }
    fn take(&mut self, n: usize) -> Result<(), Error> {
        self.remaining = self.remaining.checked_sub(n).ok_or(Error)?;
        Ok(())
    }
    fn nested(&mut self, len: Option<usize>) -> Result<&mut Self, Error> {
        if self.depth >= 256 || len.is_some_and(|n| n > self.remaining) {
            return Err(Error);
        }
        self.take(1)?;
        self.depth += 1;
        Ok(self)
    }
    fn end(&mut self) -> Result<(), Error> {
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }
}

impl Serializer for &mut Budget {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;
    fn serialize_bool(self, _: bool) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_i8(self, _: i8) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_i16(self, _: i16) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_i32(self, _: i32) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_i64(self, _: i64) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_i128(self, _: i128) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_u8(self, _: u8) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_u16(self, _: u16) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_u32(self, _: u32) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_u64(self, _: u64) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_u128(self, _: u128) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_f32(self, v: f32) -> Result<(), Error> {
        if !v.is_finite() {
            return Err(Error);
        }
        self.take(1)
    }
    fn serialize_f64(self, v: f64) -> Result<(), Error> {
        if !v.is_finite() {
            return Err(Error);
        }
        self.take(1)
    }
    fn serialize_char(self, v: char) -> Result<(), Error> {
        self.take(v.len_utf8())
    }
    fn serialize_str(self, v: &str) -> Result<(), Error> {
        self.take(v.len().saturating_add(1))
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), Error> {
        self.take(v.len().saturating_add(1))
    }
    fn serialize_none(self) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<(), Error> {
        self.take(1)?;
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Error> {
        self.take(1)
    }
    fn serialize_unit_variant(self, _: &'static str, _: u32, v: &'static str) -> Result<(), Error> {
        self.serialize_str(v)
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(self, _: &'static str, v: &T) -> Result<(), Error> {
        self.take(1)?;
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(self, _: &'static str, _: u32, name: &'static str, v: &T) -> Result<(), Error> {
        self.serialize_str(name)?;
        v.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Self, Error> {
        self.nested(len)
    }
    fn serialize_tuple(self, len: usize) -> Result<Self, Error> {
        self.nested(Some(len))
    }
    fn serialize_tuple_struct(self, _: &'static str, len: usize) -> Result<Self, Error> {
        self.nested(Some(len))
    }
    fn serialize_tuple_variant(self, _: &'static str, _: u32, name: &'static str, len: usize) -> Result<Self, Error> {
        self.serialize_str(name)?;
        self.nested(Some(len))
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Self, Error> {
        self.nested(len)
    }
    fn serialize_struct(self, _: &'static str, len: usize) -> Result<Self, Error> {
        self.nested(Some(len))
    }
    fn serialize_struct_variant(self, _: &'static str, _: u32, name: &'static str, len: usize) -> Result<Self, Error> {
        self.serialize_str(name)?;
        self.nested(Some(len))
    }
    fn collect_str<T: ?Sized + std::fmt::Display>(self, _: &T) -> Result<(), Error> {
        // All protocol strings are concrete str/String. Never allocate an unbounded Display.
        Err(Error)
    }
}
impl ser::SerializeSeq for &mut Budget {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> {
        Budget::end(self)
    }
}
impl ser::SerializeTuple for &mut Budget {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> {
        Budget::end(self)
    }
}
impl ser::SerializeTupleStruct for &mut Budget {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> {
        Budget::end(self)
    }
}
impl ser::SerializeTupleVariant for &mut Budget {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> {
        Budget::end(self)
    }
}
impl ser::SerializeMap for &mut Budget {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        v.serialize(&mut **self)
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, v: &T) -> Result<(), Error> {
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> {
        Budget::end(self)
    }
}
impl ser::SerializeStruct for &mut Budget {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, key: &'static str, v: &T) -> Result<(), Error> {
        (&mut **self).serialize_str(key)?;
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> {
        Budget::end(self)
    }
}
impl ser::SerializeStructVariant for &mut Budget {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, key: &'static str, v: &T) -> Result<(), Error> {
        (&mut **self).serialize_str(key)?;
        v.serialize(&mut **self)
    }
    fn end(self) -> Result<(), Error> {
        Budget::end(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Huge<'a>(&'a Cell<usize>);
    impl Serialize for Huge<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let _sequence = serializer.serialize_seq(Some(usize::MAX))?;
            self.0.set(self.0.get() + 1);
            Err(ser::Error::custom("must refuse before visiting any element"))
        }
    }

    #[test]
    fn large_lengths_and_nonfinite_numbers_refuse_without_visiting_elements() {
        let visited = Cell::new(0);
        assert!(Huge(&visited).serialize(&mut Budget::new(16)).is_err());
        assert_eq!(visited.get(), 0);
        assert!("this string is too large".serialize(&mut Budget::new(8)).is_err());
        assert!(f64::NAN.serialize(&mut Budget::new(8)).is_err());
        assert!(f64::INFINITY.serialize(&mut Budget::new(8)).is_err());
        assert!("small".serialize(&mut Budget::new(8)).is_ok());
    }
}
