//! MCP parameter-validation helpers.
//!
//! D3: tool params that are logically non-negative integers (`object_index`
//! etc.) used to be plain `u64`, so an agent sending `{"object_index": -1}`
//! got the raw serde error `invalid value: integer \`-1\`, expected u64` —
//! opaque and unactionable. These wrapper types accept any JSON integer,
//! validate the sign themselves, and surface an agent-friendly message
//! through the same "failed to deserialize parameters: …" channel.

use serde::{Deserialize, Deserializer, de};

macro_rules! nonneg_index_type {
    ($(#[$doc:meta])* $name:ident, $param:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, rmcp::schemars::JsonSchema)]
        #[schemars(transparent)]
        pub struct $name(u64);

        impl $name {
            pub fn get(self) -> u64 {
                self.0
            }
        }

        impl From<$name> for u64 {
            fn from(v: $name) -> u64 {
                v.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(d: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct Vis;
                impl<'de> de::Visitor<'de> for Vis {
                    type Value = $name;

                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(f, "a non-negative integer for `{}`", $param)
                    }

                    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                        if v < 0 {
                            Err(E::custom(format!(
                                "{} must be an integer ≥ 0 (0-based index into the currently \
                                 loaded dump — valid range 0..<object count>; obtain it from the \
                                 @objectId column of query results or the 'index' field of \
                                 browse_dominators results), got {v}",
                                $param
                            )))
                        } else {
                            Ok($name(v as u64))
                        }
                    }

                    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                        Ok($name(v))
                    }
                }
                d.deserialize_any(Vis)
            }
        }
    };
}

nonneg_index_type!(
    /// 0-based dense object index into the currently loaded dump's object
    /// array (the same index space as `@objectId` in query results and the
    /// `index` field in browse_dominators output).
    ObjectIndex,
    "object_index"
);

nonneg_index_type!(
    /// 0-based dense object index of the *source* object (get_field_name).
    SrcObjectIndex,
    "src_object_index"
);

nonneg_index_type!(
    /// 0-based index of one outbound edge within an object's edge list
    /// (get_field_name).
    EdgeIndex,
    "edge_index"
);
