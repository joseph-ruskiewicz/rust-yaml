//! The owned, unified YAML value tree.

use crate::meta::Meta;
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
/// `hash` remain mutually consistent.) Insert/lookup methods are added in a later task.
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

    // Internal metadata plumbing. Public metadata accessors arrive in Task 7.
    #[allow(dead_code)] // used in later tasks (and test helper in this task)
    pub(crate) fn set_meta_box(&mut self, meta: Option<Box<Meta>>) {
        self.meta = meta;
    }

    #[allow(dead_code)] // used in later tasks
    pub(crate) fn meta_box(&self) -> Option<&Meta> {
        self.meta.as_deref()
    }

    #[allow(dead_code)] // used in later tasks
    pub(crate) fn meta_box_mut(&mut self) -> &mut Box<Meta> {
        self.meta.get_or_insert_with(|| Box::new(Meta::default()))
    }

    #[allow(dead_code)] // used in later tasks
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
        self.entries.len() == other.entries.len()
            && self.entries.iter().eq(other.entries.iter())
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
        Self { entries: IndexMap::new() }
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

    pub fn iter(&self) -> indexmap::map::Iter<'_, Value, Value> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> indexmap::map::IterMut<'_, Value, Value> {
        self.entries.iter_mut()
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
        let a = Value::int(1).with_meta_for_test();
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

    impl Value {
        fn with_meta_for_test(mut self) -> Self {
            self.set_meta_box(Some(Box::new(Meta::default())));
            self
        }
    }

    #[test]
    fn mapping_preserves_insertion_order() {
        let mut m = Mapping::new();
        m.insert(Value::string("b"), Value::int(2));
        m.insert(Value::string("a"), Value::int(1));
        m.insert(Value::string("c"), Value::int(3));

        let keys: Vec<&str> = m
            .iter()
            .map(|(k, _)| k.as_str_for_test())
            .collect();
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

    impl Value {
        // Test-only helper until accessors land in Task 6.
        fn as_str_for_test(&self) -> &str {
            match self.data() {
                ValueData::String(s) => s,
                _ => panic!("not a string"),
            }
        }
    }
}
