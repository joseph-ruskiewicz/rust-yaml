//! `Value` serialization: `Serialize for Value`, plus a `Serializer` that
//! builds a `Value` (`to_value`).

use serde::ser::{self, Serialize, SerializeMap as _, SerializeSeq as _};

use crate::error::{Error, ErrorKind, Result};
use crate::value::{Mapping, Value, ValueData};

/// Converts any serializable value into the owned `Value` tree.
pub fn to_value<T: ?Sized + Serialize>(value: &T) -> Result<Value> {
    value.serialize(ValueSerializer)
}

impl Serialize for Value {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self.data() {
            ValueData::Null => serializer.serialize_unit(),
            ValueData::Bool(b) => serializer.serialize_bool(*b),
            ValueData::Int(i) => serializer.serialize_i64(*i),
            ValueData::Float(f) => serializer.serialize_f64(*f),
            ValueData::String(s) => serializer.serialize_str(s),
            ValueData::Sequence(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            ValueData::Mapping(map) => {
                let mut m = serializer.serialize_map(Some(map.len()))?;
                for (k, v) in map.iter() {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
        }
    }
}

struct ValueSerializer;

impl ser::Serializer for ValueSerializer {
    type Ok = Value;
    type Error = Error;
    type SerializeSeq = SerializeSeq;
    type SerializeTuple = SerializeSeq;
    type SerializeTupleStruct = SerializeSeq;
    type SerializeTupleVariant = SerializeTupleVariant;
    type SerializeMap = SerializeMap;
    type SerializeStruct = SerializeStruct;
    type SerializeStructVariant = SerializeStructVariant;

    fn serialize_bool(self, v: bool) -> Result<Value> {
        Ok(Value::bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<Value> {
        Ok(Value::int(v as i64))
    }
    fn serialize_i16(self, v: i16) -> Result<Value> {
        Ok(Value::int(v as i64))
    }
    fn serialize_i32(self, v: i32) -> Result<Value> {
        Ok(Value::int(v as i64))
    }
    fn serialize_i64(self, v: i64) -> Result<Value> {
        Ok(Value::int(v))
    }
    fn serialize_u8(self, v: u8) -> Result<Value> {
        Ok(Value::int(v as i64))
    }
    fn serialize_u16(self, v: u16) -> Result<Value> {
        Ok(Value::int(v as i64))
    }
    fn serialize_u32(self, v: u32) -> Result<Value> {
        Ok(Value::int(v as i64))
    }
    fn serialize_u64(self, v: u64) -> Result<Value> {
        i64::try_from(v)
            .map(Value::int)
            .map_err(|_| Error::new(ErrorKind::Compose, "u64 value exceeds i64 range"))
    }
    fn serialize_f32(self, v: f32) -> Result<Value> {
        Ok(Value::float(v as f64))
    }
    fn serialize_f64(self, v: f64) -> Result<Value> {
        Ok(Value::float(v))
    }
    fn serialize_char(self, v: char) -> Result<Value> {
        Ok(Value::string(v.to_string()))
    }
    fn serialize_str(self, v: &str) -> Result<Value> {
        Ok(Value::string(v))
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<Value> {
        Ok(Value::sequence(
            v.iter().map(|b| Value::int(*b as i64)).collect(),
        ))
    }
    fn serialize_none(self) -> Result<Value> {
        Ok(Value::null())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<Value> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<Value> {
        Ok(Value::null())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Value> {
        Ok(Value::null())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<Value> {
        Ok(Value::string(variant))
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Value> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Value> {
        let mut map = Mapping::new();
        map.insert(Value::string(variant), to_value(value)?);
        Ok(Value::mapping(map))
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<SerializeSeq> {
        Ok(SerializeSeq { items: Vec::new() })
    }
    fn serialize_tuple(self, _len: usize) -> Result<SerializeSeq> {
        Ok(SerializeSeq { items: Vec::new() })
    }
    fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<SerializeSeq> {
        Ok(SerializeSeq { items: Vec::new() })
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<SerializeTupleVariant> {
        Ok(SerializeTupleVariant {
            variant,
            items: Vec::new(),
        })
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<SerializeMap> {
        Ok(SerializeMap {
            map: Mapping::new(),
            next_key: None,
        })
    }
    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<SerializeStruct> {
        Ok(SerializeStruct {
            map: Mapping::new(),
        })
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<SerializeStructVariant> {
        Ok(SerializeStructVariant {
            variant,
            map: Mapping::new(),
        })
    }
}

pub struct SerializeSeq {
    items: Vec<Value>,
}

impl ser::SerializeSeq for SerializeSeq {
    type Ok = Value;
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.items.push(to_value(value)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        Ok(Value::sequence(self.items))
    }
}

impl ser::SerializeTuple for SerializeSeq {
    type Ok = Value;
    type Error = Error;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.items.push(to_value(value)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        Ok(Value::sequence(self.items))
    }
}

impl ser::SerializeTupleStruct for SerializeSeq {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.items.push(to_value(value)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        Ok(Value::sequence(self.items))
    }
}

pub struct SerializeTupleVariant {
    variant: &'static str,
    items: Vec<Value>,
}

impl ser::SerializeTupleVariant for SerializeTupleVariant {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.items.push(to_value(value)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        let mut map = Mapping::new();
        map.insert(Value::string(self.variant), Value::sequence(self.items));
        Ok(Value::mapping(map))
    }
}

pub struct SerializeMap {
    map: Mapping,
    next_key: Option<Value>,
}

impl ser::SerializeMap for SerializeMap {
    type Ok = Value;
    type Error = Error;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<()> {
        self.next_key = Some(to_value(key)?);
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        let key = self.next_key.take().ok_or_else(|| {
            Error::new(
                ErrorKind::Compose,
                "serialize_value called before serialize_key",
            )
        })?;
        self.map.insert(key, to_value(value)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        Ok(Value::mapping(self.map))
    }
}

pub struct SerializeStruct {
    map: Mapping,
}

impl ser::SerializeStruct for SerializeStruct {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.map.insert(Value::string(key), to_value(value)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        Ok(Value::mapping(self.map))
    }
}

pub struct SerializeStructVariant {
    variant: &'static str,
    map: Mapping,
}

impl ser::SerializeStructVariant for SerializeStructVariant {
    type Ok = Value;
    type Error = Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.map.insert(Value::string(key), to_value(value)?);
        Ok(())
    }
    fn end(self) -> Result<Value> {
        let mut outer = Mapping::new();
        outer.insert(Value::string(self.variant), Value::mapping(self.map));
        Ok(Value::mapping(outer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[test]
    fn primitives_to_value() {
        assert_eq!(to_value(&true).unwrap(), Value::bool(true));
        assert_eq!(to_value(&7i32).unwrap(), Value::int(7));
        assert_eq!(to_value(&2.5f64).unwrap(), Value::float(2.5));
        assert_eq!(to_value("hi").unwrap(), Value::string("hi"));
        assert!(to_value(&Option::<i32>::None).unwrap().is_null());
        assert_eq!(to_value(&Some(3i32)).unwrap(), Value::int(3));
    }

    #[test]
    fn vec_to_value() {
        let v = to_value(&vec![1i32, 2, 3]).unwrap();
        assert_eq!(
            v,
            Value::sequence(vec![Value::int(1), Value::int(2), Value::int(3)])
        );
    }

    #[test]
    fn struct_to_value_preserves_field_order() {
        #[derive(Serialize)]
        struct Point {
            x: i32,
            y: i32,
        }
        let v = to_value(&Point { x: 1, y: 2 }).unwrap();
        let m = v.as_mapping().unwrap();
        let keys: Vec<&str> = m.iter().map(|(k, _)| k.as_str().unwrap()).collect();
        assert_eq!(keys, ["x", "y"]);
        assert_eq!(m.get(&Value::string("x")).unwrap().as_int(), Some(1));
    }

    #[test]
    fn enum_variants_to_value() {
        #[derive(Serialize)]
        enum E {
            Unit,
            Newtype(i32),
        }
        assert_eq!(to_value(&E::Unit).unwrap(), Value::string("Unit"));
        let nt = to_value(&E::Newtype(9)).unwrap();
        assert_eq!(
            nt.as_mapping()
                .unwrap()
                .get(&Value::string("Newtype"))
                .unwrap()
                .as_int(),
            Some(9)
        );
    }

    #[test]
    fn value_serializes_through_itself() {
        let v = Value::sequence(vec![Value::int(1), Value::string("x")]);
        assert_eq!(to_value(&v).unwrap(), v);
    }
}
