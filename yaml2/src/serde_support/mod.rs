//! serde integration: convert between `T: Serialize`/`Deserialize` and `Value`.
//! Named `serde_support` (not `serde`) so it does not shadow the `serde` crate.

mod ser;

pub use ser::to_value;
