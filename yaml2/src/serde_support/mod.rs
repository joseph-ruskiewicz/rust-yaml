//! serde integration: convert between `T: Serialize`/`Deserialize` and `Value`.
//! Named `serde_support` (not `serde`) so it does not shadow the `serde` crate.

mod de;
mod ser;

pub use de::from_value;
pub use ser::to_value;
