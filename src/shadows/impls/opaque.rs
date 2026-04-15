//! ShadowNode implementations for opaque/leaf types.
//!
//! This module provides the [`impl_opaque!`] macro and ShadowNode implementations
//! for all primitive types.
//!
//! ## Why This Exists
//!
//! If primitives implement `ShadowNode` with `type Delta = Self`, then:
//! - `<u32 as ShadowNode>::Delta` = `u32`
//! - `Option<<u32 as ShadowNode>::Delta>` = `Option<u32>`
//!
//! This means codegen can uniformly use `<T as ShadowNode>::Delta` for all fields -
//! it works transparently for both primitives and nested `ShadowNode` types,
//! eliminating the need for `is_primitive()` checks in the derive macro.

/// Implement `ShadowNode`, `ReportedFields`, and (with `shadows_kv_persist`)
/// `KVPersist` for opaque/leaf types.
///
/// # Forms
///
/// ```ignore
/// // Infer KVPersist buffer size from postcard::MaxSize (type must implement it)
/// impl_opaque!(u32, bool, f64);
///
/// // Explicit buffer size — for types without postcard::MaxSize
/// impl_opaque!(core::time::Duration => 15);
/// ```
///
/// When no size is given, the `shadows_kv_persist` impl uses
/// `<T as postcard::MaxSize>::POSTCARD_MAX_SIZE` for the `ValueBuf`.
/// When a size is given, it is used directly.
#[macro_export]
macro_rules! impl_opaque {
    // Multiple types without explicit size (comma-separated list)
    ($($ty:ty),+ $(,)?) => {
        $($crate::impl_opaque!(@single $ty, <$ty as ::postcard::experimental::max_size::MaxSize>::POSTCARD_MAX_SIZE);)+
    };

    // Single type with explicit buffer size
    ($ty:ty => $size:expr) => {
        $crate::impl_opaque!(@single $ty, $size);
    };

    // Internal: generate all impls for a single type with a resolved size expression
    (@single $ty:ty, $size:expr) => {
        impl $crate::shadows::ShadowNode for $ty {
            type Delta = $ty;
            type Reported = $ty;

            const SCHEMA_HASH: u64 = $crate::shadows::fnv1a_hash(stringify!($ty).as_bytes());

            fn parse_delta<R: $crate::shadows::VariantResolver>(
                json: &[u8],
                _path: &str,
                _resolver: &R,
            ) -> impl ::core::future::Future<Output = Result<Self::Delta, $crate::shadows::ParseError>> {
                async move {
                    ::serde_json_core::from_slice(json)
                        .map(|(v, _)| v)
                        .map_err(|_| $crate::shadows::ParseError::Deserialize)
                }
            }

            fn apply_delta(&mut self, delta: &Self::Delta) {
                *self = delta.clone();
            }

            fn into_reported(&self) -> Self::Reported {
                self.clone()
            }

            fn into_partial_reported(&self, _delta: &Self::Delta) -> Self::Reported {
                self.clone()
            }
        }

        impl $crate::shadows::ReportedFields for $ty {
            const FIELD_NAMES: &'static [&'static str] = &[];

            fn serialize_into_map<S: ::serde::ser::SerializeMap>(
                &self,
                _map: &mut S,
            ) -> Result<(), S::Error> {
                Ok(())
            }
        }

        impl $crate::shadows::ReportedDiff for $ty {
            fn diff(&mut self, old: &Self) -> bool {
                *self != *old
            }
        }

        #[cfg(feature = "shadows_kv_persist")]
        impl $crate::shadows::KVPersist for $ty {
            const MAX_KEY_LEN: usize = 0;
            type ValueBuf = [u8; $size];
            fn zero_value_buf() -> Self::ValueBuf { [0u8; $size] }

            fn migration_sources(_field_path: &str) -> &'static [$crate::shadows::MigrationSource] {
                &[]
            }

            fn all_migration_keys() -> impl Iterator<Item = &'static str> {
                ::core::iter::empty()
            }

            fn apply_field_default(&mut self, _field_path: &str) -> bool {
                false
            }

            fn load_from_kv<K: $crate::shadows::KVStore, const KEY_LEN: usize>(
                &mut self,
                key_buf: &mut heapless::String<KEY_LEN>,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<$crate::shadows::LoadFieldResult, $crate::shadows::KvError<K::Error>>> {
                async move {
                    let mut result = $crate::shadows::LoadFieldResult::default();
                    let mut buf = Self::zero_value_buf();
                    match kv.fetch(key_buf.as_str(), buf.as_mut()).await.map_err($crate::shadows::KvError::Kv)? {
                        Some(data) => {
                            *self = ::postcard::from_bytes(data)
                                .map_err(|_| $crate::shadows::KvError::Serialization)?;
                            result.loaded += 1;
                        }
                        None => result.defaulted += 1,
                    }
                    Ok(result)
                }
            }

            fn load_from_kv_with_migration<K: $crate::shadows::KVStore, const KEY_LEN: usize>(
                &mut self,
                key_buf: &mut heapless::String<KEY_LEN>,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<$crate::shadows::LoadFieldResult, $crate::shadows::KvError<K::Error>>> {
                // Leaf types have no migration sources, delegate to load_from_kv
                self.load_from_kv::<K, KEY_LEN>(key_buf, kv)
            }

            fn persist_to_kv<K: $crate::shadows::KVStore, const KEY_LEN: usize>(
                &self,
                key_buf: &mut heapless::String<KEY_LEN>,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<(), $crate::shadows::KvError<K::Error>>> {
                async move {
                    let mut buf = Self::zero_value_buf();
                    let bytes = ::postcard::to_slice(self, buf.as_mut())
                        .map_err(|_| $crate::shadows::KvError::Serialization)?;
                    kv.store(key_buf.as_str(), bytes).await.map_err($crate::shadows::KvError::Kv)
                }
            }

            fn persist_delta<K: $crate::shadows::KVStore, const KEY_LEN: usize>(
                delta: &Self::Delta,
                kv: &K,
                key_buf: &mut heapless::String<KEY_LEN>,
            ) -> impl ::core::future::Future<Output = Result<(), $crate::shadows::KvError<K::Error>>> {
                async move {
                    let mut buf = Self::zero_value_buf();
                    let bytes = ::postcard::to_slice(delta, buf.as_mut())
                        .map_err(|_| $crate::shadows::KvError::Serialization)?;
                    kv.store(key_buf.as_str(), bytes).await.map_err($crate::shadows::KvError::Kv)
                }
            }

            const FIELD_COUNT: usize = 1;

            fn is_valid_key(rel_key: &str) -> bool {
                rel_key.is_empty()
            }
        }
    };
}

// =============================================================================
// Core Primitive Implementations
// =============================================================================

impl_opaque!((), bool, char);
impl_opaque!(u8, u16, u32, u64, u128, usize);
impl_opaque!(i8, i16, i32, i64, i128, isize);
impl_opaque!(f32, f64);
// postcard: u64 varint (max 10) + u32 varint (max 5) = 15 bytes
impl_opaque!(core::time::Duration => 15);

impl<T: crate::shadows::ReportedDiff> crate::shadows::ReportedDiff for Option<T> {
    fn diff(&mut self, old: &Self) -> bool {
        match (self, old) {
            (Some(new_val), Some(old_val)) => new_val.diff(old_val),
            (None, None) => false,
            _ => true,
        }
    }
}

// core::net types — postcard sizes based on non-human-readable serde encoding:
// Ipv4Addr: newtype([u8; 4]) = 4
// Ipv6Addr: newtype([u8; 16]) = 16
// IpAddr: enum(1) + Ipv6Addr(16) = 17
// SocketAddrV4: Ipv4Addr(4) + u16 varint(3) = 7
// SocketAddrV6: Ipv6Addr(16) + u16(3) + u32(5) + u32(5) = 29
// SocketAddr: enum(1) + SocketAddrV6(29) = 30
impl_opaque!(core::net::Ipv4Addr => 4);
impl_opaque!(core::net::Ipv6Addr => 16);
impl_opaque!(core::net::IpAddr => 17);
impl_opaque!(core::net::SocketAddrV4 => 7);
impl_opaque!(core::net::SocketAddrV6 => 29);
impl_opaque!(core::net::SocketAddr => 30);

#[cfg(test)]
mod tests {
    use crate::shadows::{ShadowNode, fnv1a_hash};

    #[test]
    fn test_primitive_delta_types() {
        fn assert_delta_is_self<T: ShadowNode<Delta = T>>() {}

        assert_delta_is_self::<u32>();
        assert_delta_is_self::<bool>();
        assert_delta_is_self::<f64>();
    }

    #[test]
    fn test_schema_hash_deterministic() {
        // Schema hash should be deterministic
        assert_eq!(<u32 as ShadowNode>::SCHEMA_HASH, fnv1a_hash(b"u32"));
        assert_eq!(<bool as ShadowNode>::SCHEMA_HASH, fnv1a_hash(b"bool"));
    }

    #[test]
    fn test_apply_delta() {
        let mut x: u32 = 0;
        ShadowNode::apply_delta(&mut x, &42);
        assert_eq!(x, 42);

        let mut b: bool = false;
        ShadowNode::apply_delta(&mut b, &true);
        assert!(b);
    }
}
