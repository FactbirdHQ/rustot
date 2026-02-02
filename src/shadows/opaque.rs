//! ShadowNode implementations for opaque/leaf types.
//!
//! This module provides the `impl_opaque!` macro and ShadowNode implementations
//! for all primitive types and common container types.
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

use crate::shadows::{fnv1a_hash, ReportedUnionFields, ShadowNode};
use serde::ser::SerializeMap;

#[cfg(feature = "shadows_kv_persist")]
use crate::shadows::{KVPersist, KVStore, KvError, LoadFieldResult, MigrationSource};
#[cfg(feature = "shadows_kv_persist")]
use core::future::Future;
#[cfg(feature = "shadows_kv_persist")]
use postcard::experimental::max_size::MaxSize;
#[cfg(feature = "shadows_kv_persist")]
use serde::{de::DeserializeOwned, Serialize};

/// Implement ShadowNode for opaque/leaf types.
///
/// This macro generates ShadowNode implementations where:
/// - `Delta = Self` (the type is its own delta)
/// - `Reported = Self` (the type is its own reported form)
/// - `SCHEMA_HASH = fnv1a_hash(type_name)` (type identity)
///
/// With `shadows_kv_persist` feature, also generates KVPersist impl:
/// - `MAX_DEPTH = 0` (leaf node)
/// - `MAX_KEY_LEN = 0` (no sub-keys)
/// - `MAX_VALUE_LEN = POSTCARD_MAX_SIZE` (serialized size)
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

            fn apply_delta(&mut self, delta: &Self::Delta) {
                *self = delta.clone();
            }

            fn into_reported(self) -> Self::Reported {
                self
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
            const MAX_VALUE_LEN: usize = <$ty as ::postcard::experimental::max_size::MaxSize>::POSTCARD_MAX_SIZE;

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
                    let mut buf = [0u8; Self::MAX_VALUE_LEN];
                    match kv.fetch(prefix, &mut buf).await.map_err($crate::shadows::KvError::Kv)? {
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
                    let mut buf = [0u8; Self::MAX_VALUE_LEN];
                    let bytes = ::postcard::to_slice(self, &mut buf)
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
                    let mut buf = [0u8; Self::MAX_VALUE_LEN];
                    let bytes = ::postcard::to_slice(delta, &mut buf)
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

// =============================================================================
// Generic Container Implementations (heapless)
// =============================================================================

impl<const N: usize> ShadowNode for heapless::String<N> {
    type Delta = heapless::String<N>;
    type Reported = heapless::String<N>;

    const SCHEMA_HASH: u64 = fnv1a_hash(b"heapless::String");

    fn apply_delta(&mut self, delta: &Self::Delta) {
        *self = delta.clone();
    }

    fn into_reported(self) -> Self::Reported {
        self
    }
}

impl<const N: usize> ReportedUnionFields for heapless::String<N> {
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

#[cfg(feature = "shadows_kv_persist")]
#[allow(incomplete_features)]
impl<const N: usize> KVPersist for heapless::String<N>
where
    [(); N + 5]:,
{
    const MAX_KEY_LEN: usize = 0;
    // N bytes content + varint length prefix (max 5 bytes for lengths up to u32::MAX)
    const MAX_VALUE_LEN: usize = N + 5;

    fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
        &[]
    }

    fn all_migration_keys() -> impl Iterator<Item = &'static str> {
        core::iter::empty()
    }

    fn apply_field_default(&mut self, _field_path: &str) -> bool {
        false
    }

    fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        async move {
            let mut result = LoadFieldResult::default();
            let mut buf = [0u8; N + 5];
            match kv.fetch(prefix, &mut buf).await.map_err(KvError::Kv)? {
                Some(data) => {
                    *self = postcard::from_bytes(data).map_err(|_| KvError::Serialization)?;
                    result.loaded += 1;
                }
                None => result.defaulted += 1,
            }
            Ok(result)
        }
    }

    fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        self.load_from_kv::<K, KEY_LEN>(prefix, kv)
    }

    fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            let mut buf = [0u8; N + 5];
            let bytes = postcard::to_slice(self, &mut buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
    }

    fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        prefix: &str,
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            let mut buf = [0u8; N + 5];
            let bytes = postcard::to_slice(delta, &mut buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        keys(prefix);
    }
}

impl<T, const N: usize> ShadowNode for heapless::Vec<T, N>
where
    T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
{
    type Delta = heapless::Vec<T, N>;
    type Reported = heapless::Vec<T, N>;

    const SCHEMA_HASH: u64 = fnv1a_hash(b"heapless::Vec");

    fn apply_delta(&mut self, delta: &Self::Delta) {
        *self = delta.clone();
    }

    fn into_reported(self) -> Self::Reported {
        self
    }
}

impl<T, const N: usize> ReportedUnionFields for heapless::Vec<T, N>
where
    T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
{
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

#[cfg(feature = "shadows_kv_persist")]
#[allow(incomplete_features)]
impl<T, const N: usize> KVPersist for heapless::Vec<T, N>
where
    T: Clone + Default + Serialize + DeserializeOwned + MaxSize,
    [(); N * T::POSTCARD_MAX_SIZE + 5]:,
{
    const MAX_KEY_LEN: usize = 0;
    // N * T::POSTCARD_MAX_SIZE bytes content + varint length prefix
    const MAX_VALUE_LEN: usize = N * T::POSTCARD_MAX_SIZE + 5;

    fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
        &[]
    }

    fn all_migration_keys() -> impl Iterator<Item = &'static str> {
        core::iter::empty()
    }

    fn apply_field_default(&mut self, _field_path: &str) -> bool {
        false
    }

    fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        async move {
            let mut result = LoadFieldResult::default();
            let mut buf = [0u8; N * T::POSTCARD_MAX_SIZE + 5];
            match kv.fetch(prefix, &mut buf).await.map_err(KvError::Kv)? {
                Some(data) => {
                    *self = postcard::from_bytes(data).map_err(|_| KvError::Serialization)?;
                    result.loaded += 1;
                }
                None => result.defaulted += 1,
            }
            Ok(result)
        }
    }

    fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
        &mut self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        self.load_from_kv::<K, KEY_LEN>(prefix, kv)
    }

    fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K,
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            let mut buf = [0u8; N * T::POSTCARD_MAX_SIZE + 5];
            let bytes = postcard::to_slice(self, &mut buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
    }

    fn persist_delta<K: KVStore, const KEY_LEN: usize>(
        delta: &Self::Delta,
        kv: &K,
        prefix: &str,
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            let mut buf = [0u8; N * T::POSTCARD_MAX_SIZE + 5];
            let bytes = postcard::to_slice(delta, &mut buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        keys(prefix);
    }
}

// =============================================================================
// std Feature Implementations
// =============================================================================

#[cfg(feature = "std")]
mod std_impls {
    use super::*;
    use std::string::String;
    use std::vec::Vec;

    impl ShadowNode for String {
        type Delta = String;
        type Reported = String;

        const SCHEMA_HASH: u64 = fnv1a_hash(b"String");

        fn apply_delta(&mut self, delta: &Self::Delta) {
            *self = delta.clone();
        }

        fn into_reported(self) -> Self::Reported {
            self
        }
    }

    impl ReportedUnionFields for String {
        const FIELD_NAMES: &'static [&'static str] = &[];

        fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
            Ok(())
        }
    }

    #[cfg(feature = "shadows_kv_persist")]
    impl KVPersist for String {
        const MAX_KEY_LEN: usize = 0;
        // std::String has unbounded size; unused on std path (to_allocvec/fetch_to_vec)
        const MAX_VALUE_LEN: usize = 1024;

        fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
            &[]
        }

        fn all_migration_keys() -> impl Iterator<Item = &'static str> {
            core::iter::empty()
        }

        fn apply_field_default(&mut self, _field_path: &str) -> bool {
            false
        }

        fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
            &mut self,
            prefix: &str,
            kv: &K,
        ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
            async move {
                let mut result = LoadFieldResult::default();
                match kv.fetch_to_vec(prefix).await.map_err(KvError::Kv)? {
                    Some(data) => {
                        *self = postcard::from_bytes(&data).map_err(|_| KvError::Serialization)?;
                        result.loaded += 1;
                    }
                    None => result.defaulted += 1,
                }
                Ok(result)
            }
        }

        fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
            &mut self,
            prefix: &str,
            kv: &K,
        ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
            self.load_from_kv::<K, KEY_LEN>(prefix, kv)
        }

        fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
            &self,
            prefix: &str,
            kv: &K,
        ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
            async move {
                let bytes = postcard::to_allocvec(self).map_err(|_| KvError::Serialization)?;
                kv.store(prefix, &bytes).await.map_err(KvError::Kv)
            }
        }

        fn persist_delta<K: KVStore, const KEY_LEN: usize>(
            delta: &Self::Delta,
            kv: &K,
            prefix: &str,
        ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
            async move {
                let bytes = postcard::to_allocvec(delta).map_err(|_| KvError::Serialization)?;
                kv.store(prefix, &bytes).await.map_err(KvError::Kv)
            }
        }

        fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
            keys(prefix);
        }
    }

    impl<T> ShadowNode for Vec<T>
    where
        T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
    {
        type Delta = Vec<T>;
        type Reported = Vec<T>;

        const SCHEMA_HASH: u64 = fnv1a_hash(b"Vec");

        fn apply_delta(&mut self, delta: &Self::Delta) {
            *self = delta.clone();
        }

        fn into_reported(self) -> Self::Reported {
            self
        }
    }

    impl<T> ReportedUnionFields for Vec<T>
    where
        T: Clone + Default + serde::Serialize + serde::de::DeserializeOwned,
    {
        const FIELD_NAMES: &'static [&'static str] = &[];

        fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
            Ok(())
        }
    }

    #[cfg(feature = "shadows_kv_persist")]
    impl<T> KVPersist for Vec<T>
    where
        T: Clone + Default + Serialize + DeserializeOwned,
    {
        const MAX_KEY_LEN: usize = 0;
        // std::Vec has unbounded size; unused on std path (to_allocvec/fetch_to_vec)
        const MAX_VALUE_LEN: usize = 4096;

        fn migration_sources(_field_path: &str) -> &'static [MigrationSource] {
            &[]
        }

        fn all_migration_keys() -> impl Iterator<Item = &'static str> {
            core::iter::empty()
        }

        fn apply_field_default(&mut self, _field_path: &str) -> bool {
            false
        }

        fn load_from_kv<K: KVStore, const KEY_LEN: usize>(
            &mut self,
            prefix: &str,
            kv: &K,
        ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
            async move {
                let mut result = LoadFieldResult::default();
                match kv.fetch_to_vec(prefix).await.map_err(KvError::Kv)? {
                    Some(data) => {
                        *self = postcard::from_bytes(&data).map_err(|_| KvError::Serialization)?;
                        result.loaded += 1;
                    }
                    None => result.defaulted += 1,
                }
                Ok(result)
            }
        }

        fn load_from_kv_with_migration<K: KVStore, const KEY_LEN: usize>(
            &mut self,
            prefix: &str,
            kv: &K,
        ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
            self.load_from_kv::<K, KEY_LEN>(prefix, kv)
        }

        fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
            &self,
            prefix: &str,
            kv: &K,
        ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
            async move {
                let bytes = postcard::to_allocvec(self).map_err(|_| KvError::Serialization)?;
                kv.store(prefix, &bytes).await.map_err(KvError::Kv)
            }
        }

        fn persist_delta<K: KVStore, const KEY_LEN: usize>(
            delta: &Self::Delta,
            kv: &K,
            prefix: &str,
        ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
            async move {
                let bytes = postcard::to_allocvec(delta).map_err(|_| KvError::Serialization)?;
                kv.store(prefix, &bytes).await.map_err(KvError::Kv)
            }
        }

        fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
            keys(prefix);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_primitive_delta_types() {
        // Verify that Delta types are correct
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
    fn test_into_reported_identity() {
        // into_reported should return self for primitives
        let x: u32 = 42;
        assert_eq!(ShadowNode::into_reported(x), 42);

        let b: bool = true;
        assert_eq!(ShadowNode::into_reported(b), true);
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
