//! The owned, unified YAML value tree.

use crate::meta::Meta;
use crate::meta::ScalarStyle;
use crate::options::Schema;
use core::cmp::Ordering;
use core::hash::{Hash, Hasher};
use indexmap::IndexMap;

/// The data payload of a node, without metadata.
#[derive(Debug, Clone)]
pub enum ValueData {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Sequence(Vec<Value>),
    Mapping(Mapping),
}

/// A YAML node: data plus optional, lazily-allocated formatting metadata.
#[derive(Debug, Clone)]
pub struct Value {
    data: ValueData,
    meta: Option<Box<Meta>>,
}

/// An insertion-ordered YAML mapping. Key order is always preserved.
///
/// Equality, ordering, and hashing are **insertion-order-sensitive** by design:
/// two mappings with the same entries in a different key order are considered
/// distinct. This is deliberate — this crate preserves key order as meaningful
/// data (for round-tripping), so a reordering is a different document. (`eq` and
/// `hash` remain mutually consistent.)
#[derive(Debug, Clone, Default)]
pub struct Mapping {
    entries: IndexMap<Value, Value>,
}

impl Value {
    pub fn new(data: ValueData) -> Self {
        Self { data, meta: None }
    }

    pub fn null() -> Self {
        Self::new(ValueData::Null)
    }

    pub fn bool(b: bool) -> Self {
        Self::new(ValueData::Bool(b))
    }

    pub fn int(i: i64) -> Self {
        Self::new(ValueData::Int(i))
    }

    pub fn float(f: f64) -> Self {
        Self::new(ValueData::Float(f))
    }

    pub fn string(s: impl Into<String>) -> Self {
        Self::new(ValueData::String(s.into()))
    }

    pub fn sequence(items: Vec<Value>) -> Self {
        Self::new(ValueData::Sequence(items))
    }

    pub fn mapping(m: Mapping) -> Self {
        Self::new(ValueData::Mapping(m))
    }

    pub fn data(&self) -> &ValueData {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut ValueData {
        &mut self.data
    }

    pub fn into_data(self) -> ValueData {
        self.data
    }

    // Internal metadata plumbing backing the public metadata accessors.
    pub(crate) fn set_meta_box(&mut self, meta: Option<Box<Meta>>) {
        self.meta = meta;
    }

    pub(crate) fn meta_box(&self) -> Option<&Meta> {
        self.meta.as_deref()
    }

    pub(crate) fn meta_box_mut(&mut self) -> &mut Box<Meta> {
        self.meta.get_or_insert_with(|| Box::new(Meta::default()))
    }

    pub(crate) fn take_meta_box(&mut self) -> Option<Box<Meta>> {
        self.meta.take()
    }
}

// --- Equality, ordering, hashing: data only, metadata ignored ---

impl Ord for ValueData {
    fn cmp(&self, other: &Self) -> Ordering {
        use ValueData::*;
        fn rank(v: &ValueData) -> u8 {
            match v {
                Null => 0,
                Bool(_) => 1,
                Int(_) => 2,
                Float(_) => 3,
                String(_) => 4,
                Sequence(_) => 5,
                Mapping(_) => 6,
            }
        }
        match (self, other) {
            (Null, Null) => Ordering::Equal,
            (Bool(a), Bool(b)) => a.cmp(b),
            (Int(a), Int(b)) => a.cmp(b),
            (Float(a), Float(b)) => a.total_cmp(b),
            (String(a), String(b)) => a.cmp(b),
            (Sequence(a), Sequence(b)) => a.cmp(b),
            (Mapping(a), Mapping(b)) => a.cmp(b),
            _ => rank(self).cmp(&rank(other)),
        }
    }
}

impl PartialOrd for ValueData {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for ValueData {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for ValueData {}

impl Hash for ValueData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
        match self {
            ValueData::Null => {}
            ValueData::Bool(b) => b.hash(state),
            ValueData::Int(i) => i.hash(state),
            ValueData::Float(f) => f.to_bits().hash(state),
            ValueData::String(s) => s.hash(state),
            ValueData::Sequence(s) => s.hash(state),
            ValueData::Mapping(m) => m.hash(state),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Value {
    fn cmp(&self, other: &Self) -> Ordering {
        self.data.cmp(&other.data)
    }
}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.data.hash(state);
    }
}

// Mapping equality/order/hash are entry-order-sensitive over its (key, value) pairs.

impl PartialEq for Mapping {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len() && self.entries.iter().eq(other.entries.iter())
    }
}

impl Eq for Mapping {}

impl Ord for Mapping {
    fn cmp(&self, other: &Self) -> Ordering {
        self.entries.iter().cmp(other.entries.iter())
    }
}

impl PartialOrd for Mapping {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for Mapping {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for (k, v) in &self.entries {
            k.hash(state);
            v.hash(state);
        }
    }
}

impl Mapping {
    pub fn new() -> Self {
        Self {
            entries: IndexMap::new(),
        }
    }

    pub fn insert(&mut self, key: Value, value: Value) -> Option<Value> {
        self.entries.insert(key, value)
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.entries.get(key)
    }

    pub fn get_mut(&mut self, key: &Value) -> Option<&mut Value> {
        self.entries.get_mut(key)
    }

    pub fn contains_key(&self, key: &Value) -> bool {
        self.entries.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Value, &Value)> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&Value, &mut Value)> {
        self.entries.iter_mut()
    }
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self.data, ValueData::Null)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.data {
            ValueData::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self.data {
            ValueData::Int(i) => Some(i),
            _ => None,
        }
    }

    /// Returns the value as `f64`. Integer values are widened to `f64`.
    pub fn as_float(&self) -> Option<f64> {
        match self.data {
            ValueData::Float(f) => Some(f),
            ValueData::Int(i) => Some(i as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.data {
            ValueData::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_sequence(&self) -> Option<&[Value]> {
        match &self.data {
            ValueData::Sequence(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_mapping(&self) -> Option<&Mapping> {
        match &self.data {
            ValueData::Mapping(m) => Some(m),
            _ => None,
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::bool(b)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::int(i)
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::float(f)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::string(s)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::string(s)
    }
}

impl Value {
    /// Returns the formatting metadata if any has been attached.
    pub fn meta(&self) -> Option<&Meta> {
        self.meta_box()
    }

    /// Returns the metadata mutably, allocating an empty `Meta` on first access.
    pub fn meta_mut(&mut self) -> &mut Meta {
        self.meta_box_mut()
    }

    /// Attaches metadata, returning the value for chaining.
    pub fn with_meta(mut self, meta: Meta) -> Self {
        self.set_meta_box(Some(Box::new(meta)));
        self
    }

    /// Removes and returns any attached metadata.
    pub fn take_meta(&mut self) -> Option<Meta> {
        self.take_meta_box().map(|b| *b)
    }
}

impl Value {
    /// Builds a scalar value by resolving raw source text under the given style
    /// and schema. Quoted/literal/folded styles always yield a string.
    pub fn from_scalar(raw: &str, style: ScalarStyle, schema: Schema) -> Value {
        Value::new(crate::scalar::resolve(raw, style, schema))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_constructors_build_expected_data() {
        assert!(matches!(Value::null().data(), ValueData::Null));
        assert!(matches!(Value::bool(true).data(), ValueData::Bool(true)));
        assert!(matches!(Value::int(7).data(), ValueData::Int(7)));
        assert!(matches!(Value::string("hi").data(), ValueData::String(s) if s == "hi"));
    }

    #[test]
    fn equality_ignores_metadata() {
        let mut a = Value::int(1);
        a.meta_mut(); // force-allocate metadata
        let b = Value::int(1);
        assert_eq!(a, b);
    }

    #[test]
    fn distinct_scalars_are_not_equal() {
        assert_ne!(Value::int(1), Value::int(2));
        assert_ne!(Value::int(1), Value::string("1"));
    }

    #[test]
    fn float_zero_signs_are_distinct_keys() {
        // Consistent Eq/Hash: +0.0 and -0.0 differ by bits and must not collide.
        assert_ne!(Value::float(0.0), Value::float(-0.0));
    }

    #[test]
    fn cross_variant_ordering_follows_rank() {
        // Null < Bool < Int < Float < String < Sequence < Mapping
        assert!(Value::null() < Value::bool(false));
        assert!(Value::bool(true) < Value::int(0));
        assert!(Value::int(99) < Value::float(0.0));
        assert!(Value::float(1.0) < Value::string("a"));
        assert!(Value::string("z") < Value::sequence(vec![]));
    }

    #[test]
    fn sequence_ordering_is_lexicographic() {
        let a = Value::sequence(vec![Value::int(1), Value::int(2)]);
        let b = Value::sequence(vec![Value::int(1), Value::int(3)]);
        assert!(a < b);
    }

    #[test]
    fn mapping_equality_is_order_sensitive() {
        use indexmap::IndexMap;
        let mut e1 = IndexMap::new();
        e1.insert(Value::string("a"), Value::int(1));
        e1.insert(Value::string("b"), Value::int(2));
        let mut e2 = IndexMap::new();
        e2.insert(Value::string("b"), Value::int(2));
        e2.insert(Value::string("a"), Value::int(1));
        let m1 = Mapping { entries: e1 };
        let m2 = Mapping { entries: e2 };
        // Same entries, different order -> not equal (deliberate).
        assert_ne!(Value::mapping(m1), Value::mapping(m2));
    }

    #[test]
    fn mapping_preserves_insertion_order() {
        let mut m = Mapping::new();
        m.insert(Value::string("b"), Value::int(2));
        m.insert(Value::string("a"), Value::int(1));
        m.insert(Value::string("c"), Value::int(3));

        let keys: Vec<&str> = m.iter().map(|(k, _)| k.as_str().unwrap()).collect();
        assert_eq!(keys, ["b", "a", "c"]);
    }

    #[test]
    fn mapping_get_and_len() {
        let mut m = Mapping::new();
        assert!(m.is_empty());
        m.insert(Value::string("k"), Value::int(9));
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&Value::string("k")), Some(&Value::int(9)));
        assert_eq!(m.get(&Value::string("missing")), None);
    }

    #[test]
    fn accessors_return_typed_views() {
        assert!(Value::null().is_null());
        assert_eq!(Value::bool(true).as_bool(), Some(true));
        assert_eq!(Value::int(5).as_int(), Some(5));
        assert_eq!(Value::int(5).as_float(), Some(5.0));
        assert_eq!(Value::float(1.5).as_float(), Some(1.5));
        assert_eq!(Value::string("x").as_str(), Some("x"));
        assert_eq!(Value::int(5).as_str(), None);
    }

    #[test]
    fn sequence_and_mapping_accessors() {
        let seq = Value::sequence(vec![Value::int(1), Value::int(2)]);
        assert_eq!(seq.as_sequence().unwrap().len(), 2);

        let mut m = Mapping::new();
        m.insert(Value::string("k"), Value::int(1));
        let map = Value::mapping(m);
        assert_eq!(map.as_mapping().unwrap().len(), 1);
    }

    #[test]
    fn from_conversions() {
        assert_eq!(Value::from(true), Value::bool(true));
        assert_eq!(Value::from(3_i64), Value::int(3));
        assert_eq!(Value::from(2.5_f64), Value::float(2.5));
        assert_eq!(Value::from("s"), Value::string("s"));
        assert_eq!(Value::from(String::from("s")), Value::string("s"));
    }

    #[test]
    fn meta_is_absent_until_requested() {
        let v = Value::int(1);
        assert!(v.meta().is_none());
    }

    #[test]
    fn meta_mut_lazily_allocates() {
        let mut v = Value::int(1);
        v.meta_mut().anchor = Some("a1".to_string());
        assert_eq!(v.meta().unwrap().anchor.as_deref(), Some("a1"));
    }

    #[test]
    fn with_meta_sets_and_take_meta_removes() {
        let meta = Meta {
            tag: Some("!!str".to_string()),
            ..Meta::default()
        };
        let mut v = Value::string("x").with_meta(meta);
        assert!(v.meta().is_some());
        let taken = v.take_meta().unwrap();
        assert_eq!(taken.tag.as_deref(), Some("!!str"));
        assert!(v.meta().is_none());
    }

    #[test]
    fn from_scalar_resolves_per_schema() {
        let core = Value::from_scalar("0777", ScalarStyle::Plain, Schema::Core1_2);
        assert_eq!(core.as_int(), Some(777));

        let y11 = Value::from_scalar("0777", ScalarStyle::Plain, Schema::Yaml1_1);
        assert_eq!(y11.as_int(), Some(511));

        let quoted = Value::from_scalar("true", ScalarStyle::SingleQuoted, Schema::Core1_2);
        assert_eq!(quoted.as_str(), Some("true"));
    }
}
