//! `Value` deserialization: `Deserialize for Value`, plus a `Deserializer` that
//! reads a `Value` (`from_value`).

use serde::de::{
    self, Deserialize, DeserializeSeed, Deserializer, EnumAccess, IntoDeserializer, MapAccess,
    SeqAccess, VariantAccess, Visitor,
};

use crate::error::{Error, ErrorKind, Result};
use crate::value::{Mapping, Value, ValueData};

/// Converts an owned `Value` into any deserializable type.
pub fn from_value<T: de::DeserializeOwned>(value: Value) -> Result<T> {
    T::deserialize(value)
}

impl<'de> IntoDeserializer<'de, Error> for Value {
    type Deserializer = Self;
    fn into_deserializer(self) -> Self {
        self
    }
}

impl<'de> Deserializer<'de> for Value {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        match self.into_data() {
            ValueData::Null => visitor.visit_unit(),
            ValueData::Bool(b) => visitor.visit_bool(b),
            ValueData::Int(i) => visitor.visit_i64(i),
            ValueData::Float(f) => visitor.visit_f64(f),
            ValueData::String(s) => visitor.visit_string(s),
            ValueData::Sequence(items) => visitor.visit_seq(SeqAccessImpl {
                iter: items.into_iter(),
            }),
            ValueData::Mapping(map) => visitor.visit_map(MapAccessImpl {
                iter: map.into_iter(),
                value: None,
            }),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value> {
        if self.is_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        match self.into_data() {
            ValueData::String(variant) => visitor.visit_enum(EnumImpl {
                variant,
                value: None,
            }),
            ValueData::Mapping(map) => {
                let mut iter = map.into_iter();
                let (key, value) = iter.next().ok_or_else(|| {
                    Error::new(
                        ErrorKind::Compose,
                        "expected a single-key mapping for an enum",
                    )
                })?;
                if iter.next().is_some() {
                    return Err(Error::new(
                        ErrorKind::Compose,
                        "expected a single-key mapping for an enum",
                    ));
                }
                let variant = match key.into_data() {
                    ValueData::String(s) => s,
                    _ => {
                        return Err(Error::new(
                            ErrorKind::Compose,
                            "enum variant key must be a string",
                        ))
                    }
                };
                visitor.visit_enum(EnumImpl {
                    variant,
                    value: Some(value),
                })
            }
            _ => Err(Error::new(
                ErrorKind::Compose,
                "expected a string or single-key mapping for an enum",
            )),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct
        identifier ignored_any
    }
}

struct SeqAccessImpl {
    iter: std::vec::IntoIter<Value>,
}

impl<'de> SeqAccess<'de> for SeqAccessImpl {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> Result<Option<T::Value>> {
        match self.iter.next() {
            Some(v) => seed.deserialize(v).map(Some),
            None => Ok(None),
        }
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct MapAccessImpl {
    iter: indexmap::map::IntoIter<Value, Value>,
    value: Option<Value>,
}

impl<'de> MapAccess<'de> for MapAccessImpl {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>> {
        match self.iter.next() {
            Some((k, v)) => {
                self.value = Some(v);
                seed.deserialize(k).map(Some)
            }
            None => Ok(None),
        }
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value> {
        let value = self
            .value
            .take()
            .ok_or_else(|| Error::new(ErrorKind::Compose, "next_value before next_key"))?;
        seed.deserialize(value)
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct EnumImpl {
    variant: String,
    value: Option<Value>,
}

impl<'de> EnumAccess<'de> for EnumImpl {
    type Error = Error;
    type Variant = VariantImpl;
    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, VariantImpl)> {
        let variant = seed.deserialize(self.variant.into_deserializer())?;
        Ok((variant, VariantImpl { value: self.value }))
    }
}

struct VariantImpl {
    value: Option<Value>,
}

impl<'de> VariantAccess<'de> for VariantImpl {
    type Error = Error;
    fn unit_variant(self) -> Result<()> {
        match self.value {
            None => Ok(()),
            Some(_) => Err(Error::new(
                ErrorKind::Compose,
                "unexpected payload for a unit variant",
            )),
        }
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value> {
        match self.value {
            Some(v) => seed.deserialize(v),
            None => Err(Error::new(
                ErrorKind::Compose,
                "expected a value for a newtype variant",
            )),
        }
    }
    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, visitor: V) -> Result<V::Value> {
        match self.value {
            Some(v) => v.deserialize_seq(visitor),
            None => Err(Error::new(
                ErrorKind::Compose,
                "expected a sequence for a tuple variant",
            )),
        }
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value> {
        match self.value {
            Some(v) => v.deserialize_map(visitor),
            None => Err(Error::new(
                ErrorKind::Compose,
                "expected a mapping for a struct variant",
            )),
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Value, D::Error> {
        deserializer.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("any YAML value")
    }

    fn visit_bool<E>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(Value::bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> std::result::Result<Value, E> {
        Ok(Value::int(v))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Value, E> {
        i64::try_from(v)
            .map(Value::int)
            .map_err(|_| E::custom("u64 value exceeds i64 range"))
    }
    fn visit_f64<E>(self, v: f64) -> std::result::Result<Value, E> {
        Ok(Value::float(v))
    }
    fn visit_str<E>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(Value::string(v))
    }
    fn visit_string<E>(self, v: String) -> std::result::Result<Value, E> {
        Ok(Value::string(v))
    }
    fn visit_none<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::null())
    }
    fn visit_unit<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::null())
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Value, D::Error> {
        Deserialize::deserialize(d)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(e) = seq.next_element()? {
            items.push(e);
        }
        Ok(Value::sequence(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut m = Mapping::new();
        while let Some((k, v)) = map.next_entry()? {
            m.insert(k, v);
        }
        Ok(Value::mapping(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[test]
    fn primitives_from_value() {
        assert_eq!(from_value::<i32>(Value::int(5)).unwrap(), 5);
        assert!(from_value::<bool>(Value::bool(true)).unwrap());
        assert_eq!(from_value::<f64>(Value::float(1.5)).unwrap(), 1.5);
        assert_eq!(from_value::<String>(Value::string("x")).unwrap(), "x");
    }

    #[test]
    fn option_from_null_and_value() {
        assert_eq!(from_value::<Option<i32>>(Value::null()).unwrap(), None);
        assert_eq!(from_value::<Option<i32>>(Value::int(7)).unwrap(), Some(7));
    }

    #[test]
    fn vec_from_value() {
        let v = Value::sequence(vec![Value::int(1), Value::int(2)]);
        assert_eq!(from_value::<Vec<i32>>(v).unwrap(), vec![1, 2]);
    }

    #[test]
    fn struct_from_value() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct Point {
            x: i32,
            y: i32,
        }
        let mut m = Mapping::new();
        m.insert(Value::string("x"), Value::int(1));
        m.insert(Value::string("y"), Value::int(2));
        assert_eq!(
            from_value::<Point>(Value::mapping(m)).unwrap(),
            Point { x: 1, y: 2 }
        );
    }

    #[test]
    fn enum_from_value() {
        #[derive(Deserialize, Debug, PartialEq)]
        enum E {
            Unit,
            Newtype(i32),
        }
        assert_eq!(from_value::<E>(Value::string("Unit")).unwrap(), E::Unit);
        let mut m = Mapping::new();
        m.insert(Value::string("Newtype"), Value::int(9));
        assert_eq!(from_value::<E>(Value::mapping(m)).unwrap(), E::Newtype(9));
    }

    #[test]
    fn value_deserializes_through_itself() {
        let v = Value::sequence(vec![Value::int(1), Value::string("x")]);
        assert_eq!(from_value::<Value>(v.clone()).unwrap(), v);
    }
}
