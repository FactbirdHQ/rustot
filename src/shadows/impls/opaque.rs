//! ShadowNode implementations for opaque/leaf types.
//!
//! This module provides the `impl_opaque!` macro and ShadowNode implementations
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

/// Implement ShadowNode for opaque/leaf types.
///
/// This macro generates ShadowNode implementations where:
/// - `Delta = Self` (the type is its own delta)
/// - `Reported = Self` (the type is its own reported form)
/// - `SCHEMA_HASH = fnv1a_hash(type_name)` (type identity)
///
/// With `shadows_kv_persist` feature, also generates KVPersist impl:
/// - `MAX_KEY_LEN = 0` (no sub-keys)
/// - `ValueBuf = [u8; POSTCARD_MAX_SIZE]` (serialization buffer)
///
/// # Example
///
/// ```ignore
/// impl_opaque!(MyCustomType);
/// ```
#[macro_export]
macro_rules! impl_opaque {
    ($($ty:ty),* $(,)?) => {$(
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

            fn into_partial_reported(&self, _delta: &Self::Delta) -> Self::Reported {
                self.clone()
            }
        }

        impl $crate::shadows::ReportedUnionFields for $ty {
            const FIELD_NAMES: &'static [&'static str] = &[];

            fn serialize_into_map<S: ::serde::ser::SerializeMap>(
                &self,
                _map: &mut S,
            ) -> Result<(), S::Error> {
                Ok(())
            }
        }

        #[cfg(feature = "shadows_kv_persist")]
        impl $crate::shadows::KVPersist for $ty {
            const MAX_KEY_LEN: usize = 0;
            type ValueBuf = [u8; <$ty as ::postcard::experimental::max_size::MaxSize>::POSTCARD_MAX_SIZE];
            fn zero_value_buf() -> Self::ValueBuf { [0u8; <$ty as ::postcard::experimental::max_size::MaxSize>::POSTCARD_MAX_SIZE] }

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
                prefix: &str,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<$crate::shadows::LoadFieldResult, $crate::shadows::KvError<K::Error>>> {
                async move {
                    let mut result = $crate::shadows::LoadFieldResult::default();
                    let mut buf = Self::zero_value_buf();
                    match kv.fetch(prefix, buf.as_mut()).await.map_err($crate::shadows::KvError::Kv)? {
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
                prefix: &str,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<$crate::shadows::LoadFieldResult, $crate::shadows::KvError<K::Error>>> {
                // Leaf types have no migration sources, delegate to load_from_kv
                self.load_from_kv::<K, KEY_LEN>(prefix, kv)
            }

            fn persist_to_kv<K: $crate::shadows::KVStore, const KEY_LEN: usize>(
                &self,
                prefix: &str,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<(), $crate::shadows::KvError<K::Error>>> {
                async move {
                    let mut buf = Self::zero_value_buf();
                    let bytes = ::postcard::to_slice(self, buf.as_mut())
                        .map_err(|_| $crate::shadows::KvError::Serialization)?;
                    kv.store(prefix, bytes).await.map_err($crate::shadows::KvError::Kv)
                }
            }

            fn persist_delta<K: $crate::shadows::KVStore, const KEY_LEN: usize>(
                delta: &Self::Delta,
                kv: &K,
                prefix: &str,
            ) -> impl ::core::future::Future<Output = Result<(), $crate::shadows::KvError<K::Error>>> {
                async move {
                    let mut buf = Self::zero_value_buf();
                    let bytes = ::postcard::to_slice(delta, buf.as_mut())
                        .map_err(|_| $crate::shadows::KvError::Serialization)?;
                    kv.store(prefix, bytes).await.map_err($crate::shadows::KvError::Kv)
                }
            }

            fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
                keys(prefix);
            }
        }
    )*};
}

// =============================================================================
// Core Primitive Implementations
// =============================================================================

impl_opaque!((), bool, char);
impl_opaque!(u8, u16, u32, u64, u128, usize);
impl_opaque!(i8, i16, i32, i64, i128, isize);
impl_opaque!(f32, f64);
// core::time::Duration — manual impl because it lacks postcard::MaxSize.
// Serialized as (u64, u32) = secs + nanos, max postcard size = 10 + 5 = 15 bytes.

impl crate::shadows::ShadowNode for core::time::Duration {
    type Delta = core::time::Duration;
    type Reported = core::time::Duration;

    const SCHEMA_HASH: u64 = crate::shadows::fnv1a_hash(b"core::time::Duration");

    async fn parse_delta<R: crate::shadows::VariantResolver>(
        json: &[u8],
        _path: &str,
        _resolver: &R,
    ) -> Result<Self::Delta, crate::shadows::ParseError> {
        ::serde_json_core::from_slice(json)
            .map(|(v, _)| v)
            .map_err(|_| crate::shadows::ParseError::Deserialize)
    }

    fn apply_delta(&mut self, delta: &Self::Delta) {
        *self = *delta;
    }

    fn into_partial_reported(&self, _delta: &Self::Delta) -> Self::Reported {
        *self
    }
}

impl crate::shadows::ReportedUnionFields for core::time::Duration {
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: ::serde::ser::SerializeMap>(
        &self,
        _map: &mut S,
    ) -> Result<(), S::Error> {
        Ok(())
    }
}

#[cfg(feature = "shadows_kv_persist")]
impl crate::shadows::KVPersist for core::time::Duration {
    const MAX_KEY_LEN: usize = 0;
    // postcard: u64 varint (max 10) + u32 varint (max 5) = 15 bytes
    type ValueBuf = [u8; 15];
    fn zero_value_buf() -> Self::ValueBuf {
        [0u8; 15]
    }

    fn migration_sources(_field_path: &str) -> &'static [crate::shadows::MigrationSource] {
        &[]
    }

    fn all_migration_keys() -> impl Iterator<Item = &'static str> {
        core::iter::empty()
    }

    fn apply_field_default(&mut self, _field_path: &str) -> bool {
        false
    }

    async fn load_from_kv<K: crate::shadows::KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> Result<crate::shadows::LoadFieldResult, crate::shadows::KvError<K::Error>> {
        let mut result = crate::shadows::LoadFieldResult::default();
        let mut buf = Self::zero_value_buf();
        match kv
            .fetch(prefix, buf.as_mut())
            .await
            .map_err(crate::shadows::KvError::Kv)?
        {
            Some(data) => {
                *self = ::postcard::from_bytes(data)
                    .map_err(|_| crate::shadows::KvError::Serialization)?;
                result.loaded += 1;
            }
            None => result.defaulted += 1,
        }
        Ok(result)
    }

    async fn load_from_kv_with_migration<K: crate::shadows::KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> Result<crate::shadows::LoadFieldResult, crate::shadows::KvError<K::Error>> {
        self.load_from_kv::<K, KEY_LEN>(prefix, kv).await
    }

    async fn persist_to_kv<K: crate::shadows::KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K,
    ) -> Result<(), crate::shadows::KvError<K::Error>> {
        let mut buf = Self::zero_value_buf();
        let bytes = ::postcard::to_slice(self, buf.as_mut())
            .map_err(|_| crate::shadows::KvError::Serialization)?;
        kv.store(prefix, bytes)
            .await
            .map_err(crate::shadows::KvError::Kv)
    }

    async fn persist_delta<K: crate::shadows::KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        prefix: &str,
    ) -> Result<(), crate::shadows::KvError<K::Error>> {
        let mut buf = Self::zero_value_buf();
        let bytes = ::postcard::to_slice(delta, buf.as_mut())
            .map_err(|_| crate::shadows::KvError::Serialization)?;
        kv.store(prefix, bytes)
            .await
            .map_err(crate::shadows::KvError::Kv)
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        keys(prefix);
    }
}

#[cfg(test)]
mod tests {
    use crate::shadows::{fnv1a_hash, ShadowNode};

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
