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

use crate::shadows::{
    fnv1a_hash, KVStore, KvError, LoadFieldResult, MigrationSource, ReportedUnionFields,
    ShadowNode, ShadowPatch,
};
use core::future::Future;
use postcard::experimental::max_size::MaxSize;
use serde::{de::DeserializeOwned, ser::SerializeMap, Serialize};

/// Implement ShadowNode for opaque/leaf types.
///
/// This macro generates ShadowNode implementations where:
/// - `Delta = Self` (the type is its own delta)
/// - `Reported = Self` (the type is its own reported form)
/// - `MAX_DEPTH = 0` (leaf node)
/// - `MAX_KEY_LEN = 0` (no sub-keys)
/// - `MAX_VALUE_LEN = POSTCARD_MAX_SIZE` (serialized size)
/// - `SCHEMA_HASH = fnv1a_hash(type_name)` (type identity)
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

            const MAX_DEPTH: usize = 0;
            const MAX_KEY_LEN: usize = 0;
            const MAX_VALUE_LEN: usize = <$ty as ::postcard::experimental::max_size::MaxSize>::POSTCARD_MAX_SIZE;
            const SCHEMA_HASH: u64 = $crate::shadows::fnv1a_hash(stringify!($ty).as_bytes());

            fn apply_and_persist<K: $crate::shadows::KVStore>(
                &mut self,
                delta: &Self::Delta,
                prefix: &str,
                kv: &K,
                buf: &mut [u8],
            ) -> impl ::core::future::Future<Output = Result<(), $crate::shadows::KvError<K::Error>>> {
                async move {
                    *self = delta.clone();
                    let bytes = ::postcard::to_slice(self, buf)
                        .map_err(|_| $crate::shadows::KvError::Serialization)?;
                    kv.store(prefix, bytes).await.map_err($crate::shadows::KvError::Kv)
                }
            }

            fn into_reported(self) -> Self::Reported {
                self
            }

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
                buf: &mut [u8],
            ) -> impl ::core::future::Future<Output = Result<$crate::shadows::LoadFieldResult, $crate::shadows::KvError<K::Error>>> {
                async move {
                    let mut result = $crate::shadows::LoadFieldResult::default();
                    match kv.fetch(prefix, buf).await.map_err($crate::shadows::KvError::Kv)? {
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
                buf: &mut [u8],
            ) -> impl ::core::future::Future<Output = Result<$crate::shadows::LoadFieldResult, $crate::shadows::KvError<K::Error>>> {
                // Leaf types have no migration sources, delegate to load_from_kv
                self.load_from_kv::<K, KEY_LEN>(prefix, kv, buf)
            }

            fn persist_to_kv<K: $crate::shadows::KVStore, const KEY_LEN: usize>(
                &self,
                prefix: &str,
                kv: &K,
                buf: &mut [u8],
            ) -> impl ::core::future::Future<Output = Result<(), $crate::shadows::KvError<K::Error>>> {
                async move {
                    let bytes = ::postcard::to_slice(self, buf)
                        .map_err(|_| $crate::shadows::KvError::Serialization)?;
                    kv.store(prefix, bytes).await.map_err($crate::shadows::KvError::Kv)
                }
            }

            fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
                keys(prefix);
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

        impl $crate::shadows::ShadowPatch for $ty {
            type Delta = $ty;
            type Reported = $ty;

            fn apply_patch(&mut self, delta: Self::Delta) {
                *self = delta;
            }

            fn into_reported(self) -> Self::Reported {
                self
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

    const MAX_DEPTH: usize = 0;
    const MAX_KEY_LEN: usize = 0;
    // heapless::String<N> serializes as a string with max N bytes + length prefix
    const MAX_VALUE_LEN: usize = N + 5; // +5 for postcard length encoding overhead
    const SCHEMA_HASH: u64 = fnv1a_hash(b"heapless::String");

    fn apply_and_persist<K: KVStore>(
        &mut self,
        delta: &Self::Delta,
        prefix: &str,
        kv: &K,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            *self = delta.clone();
            let bytes = postcard::to_slice(self, buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
    }

    fn into_reported(self) -> Self::Reported {
        self
    }

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
        buf: &mut [u8],
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        async move {
            let mut result = LoadFieldResult::default();
            match kv.fetch(prefix, buf).await.map_err(KvError::Kv)? {
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
        buf: &mut [u8],
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        self.load_from_kv::<K, KEY_LEN>(prefix, kv, buf)
    }

    fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            let bytes = postcard::to_slice(self, buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        keys(prefix);
    }
}

impl<const N: usize> ReportedUnionFields for heapless::String<N> {
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

impl<const N: usize> ShadowPatch for heapless::String<N> {
    type Delta = heapless::String<N>;
    type Reported = heapless::String<N>;

    fn apply_patch(&mut self, delta: Self::Delta) {
        *self = delta;
    }

    fn into_reported(self) -> Self::Reported {
        self
    }
}

impl<T, const N: usize> ShadowNode for heapless::Vec<T, N>
where
    T: Clone + Default + Serialize + DeserializeOwned + MaxSize,
{
    type Delta = heapless::Vec<T, N>;
    type Reported = heapless::Vec<T, N>;

    const MAX_DEPTH: usize = 0;
    const MAX_KEY_LEN: usize = 0;
    // Vec<T, N> serializes as: length prefix + N * T::MAX_SIZE
    const MAX_VALUE_LEN: usize = 5 + N * T::POSTCARD_MAX_SIZE;
    const SCHEMA_HASH: u64 = fnv1a_hash(b"heapless::Vec");

    fn apply_and_persist<K: KVStore>(
        &mut self,
        delta: &Self::Delta,
        prefix: &str,
        kv: &K,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            *self = delta.clone();
            let bytes = postcard::to_slice(self, buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
    }

    fn into_reported(self) -> Self::Reported {
        self
    }

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
        buf: &mut [u8],
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        async move {
            let mut result = LoadFieldResult::default();
            match kv.fetch(prefix, buf).await.map_err(KvError::Kv)? {
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
        buf: &mut [u8],
    ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
        self.load_from_kv::<K, KEY_LEN>(prefix, kv, buf)
    }

    fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
        &self,
        prefix: &str,
        kv: &K,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
        async move {
            let bytes = postcard::to_slice(self, buf).map_err(|_| KvError::Serialization)?;
            kv.store(prefix, bytes).await.map_err(KvError::Kv)
        }
    }

    fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
        keys(prefix);
    }
}

impl<T, const N: usize> ReportedUnionFields for heapless::Vec<T, N>
where
    T: Clone + Default + Serialize + DeserializeOwned + MaxSize,
{
    const FIELD_NAMES: &'static [&'static str] = &[];

    fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
        Ok(())
    }
}

impl<T, const N: usize> ShadowPatch for heapless::Vec<T, N>
where
    T: Clone + Default + Serialize + DeserializeOwned + MaxSize,
{
    type Delta = heapless::Vec<T, N>;
    type Reported = heapless::Vec<T, N>;

    fn apply_patch(&mut self, delta: Self::Delta) {
        *self = delta;
    }

    fn into_reported(self) -> Self::Reported {
        self
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

        const MAX_DEPTH: usize = 0;
        const MAX_KEY_LEN: usize = 0;
        // std::String has unbounded size, use a reasonable max
        const MAX_VALUE_LEN: usize = 1024;
        const SCHEMA_HASH: u64 = fnv1a_hash(b"String");

        fn apply_and_persist<K: KVStore>(
            &mut self,
            delta: &Self::Delta,
            prefix: &str,
            kv: &K,
            buf: &mut [u8],
        ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
            async move {
                *self = delta.clone();
                let bytes = postcard::to_slice(self, buf).map_err(|_| KvError::Serialization)?;
                kv.store(prefix, bytes).await.map_err(KvError::Kv)
            }
        }

        fn into_reported(self) -> Self::Reported {
            self
        }

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
            buf: &mut [u8],
        ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
            async move {
                let mut result = LoadFieldResult::default();
                match kv.fetch(prefix, buf).await.map_err(KvError::Kv)? {
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
            buf: &mut [u8],
        ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
            self.load_from_kv::<K, KEY_LEN>(prefix, kv, buf)
        }

        fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
            &self,
            prefix: &str,
            kv: &K,
            buf: &mut [u8],
        ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
            async move {
                let bytes = postcard::to_slice(self, buf).map_err(|_| KvError::Serialization)?;
                kv.store(prefix, bytes).await.map_err(KvError::Kv)
            }
        }

        fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
            keys(prefix);
        }
    }

    impl ReportedUnionFields for String {
        const FIELD_NAMES: &'static [&'static str] = &[];

        fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
            Ok(())
        }
    }

    // Note: ShadowPatch for String is in alloc_impl.rs

    impl<T> ShadowNode for Vec<T>
    where
        T: Clone + Default + Serialize + DeserializeOwned,
    {
        type Delta = Vec<T>;
        type Reported = Vec<T>;

        const MAX_DEPTH: usize = 0;
        const MAX_KEY_LEN: usize = 0;
        // std::Vec has unbounded size, use a reasonable max
        const MAX_VALUE_LEN: usize = 4096;
        const SCHEMA_HASH: u64 = fnv1a_hash(b"Vec");

        fn apply_and_persist<K: KVStore>(
            &mut self,
            delta: &Self::Delta,
            prefix: &str,
            kv: &K,
            buf: &mut [u8],
        ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
            async move {
                *self = delta.clone();
                let bytes = postcard::to_slice(self, buf).map_err(|_| KvError::Serialization)?;
                kv.store(prefix, bytes).await.map_err(KvError::Kv)
            }
        }

        fn into_reported(self) -> Self::Reported {
            self
        }

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
            buf: &mut [u8],
        ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
            async move {
                let mut result = LoadFieldResult::default();
                match kv.fetch(prefix, buf).await.map_err(KvError::Kv)? {
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
            buf: &mut [u8],
        ) -> impl Future<Output = Result<LoadFieldResult, KvError<K::Error>>> {
            self.load_from_kv::<K, KEY_LEN>(prefix, kv, buf)
        }

        fn persist_to_kv<K: KVStore, const KEY_LEN: usize>(
            &self,
            prefix: &str,
            kv: &K,
            buf: &mut [u8],
        ) -> impl Future<Output = Result<(), KvError<K::Error>>> {
            async move {
                let bytes = postcard::to_slice(self, buf).map_err(|_| KvError::Serialization)?;
                kv.store(prefix, bytes).await.map_err(KvError::Kv)
            }
        }

        fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
            keys(prefix);
        }
    }

    impl<T> ReportedUnionFields for Vec<T>
    where
        T: Clone + Default + Serialize + DeserializeOwned,
    {
        const FIELD_NAMES: &'static [&'static str] = &[];

        fn serialize_into_map<S: SerializeMap>(&self, _map: &mut S) -> Result<(), S::Error> {
            Ok(())
        }
    }

    // Note: ShadowPatch for Vec is in alloc_impl.rs
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
    fn test_max_depth_zero() {
        // All opaque types have MAX_DEPTH = 0
        assert_eq!(<u32 as ShadowNode>::MAX_DEPTH, 0);
        assert_eq!(<bool as ShadowNode>::MAX_DEPTH, 0);
        assert_eq!(<f64 as ShadowNode>::MAX_DEPTH, 0);
    }

    #[test]
    fn test_into_reported_identity() {
        // into_reported should return self for primitives
        let x: u32 = 42;
        assert_eq!(ShadowNode::into_reported(x), 42);

        let b: bool = true;
        assert_eq!(ShadowNode::into_reported(b), true);
    }
}
