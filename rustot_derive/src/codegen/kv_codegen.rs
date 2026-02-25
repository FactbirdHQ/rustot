//! KV persistence code generation helpers
//!
//! This module provides reusable code generation patterns for KVPersist trait implementations.
//! These helpers eliminate duplication between struct, enum, and adjacently-tagged enum codegen.
//!
//! The generated code always uses fixed-size buffers (`zero_value_buf()` + `kv.fetch()` +
//! `postcard::to_slice()`). Types that need allocating paths (std String, Vec, HashMap) handle
//! that internally in their own KVPersist impls — the codegen never emits `#[cfg(feature)]`
//! blocks, which would be evaluated in the user's crate instead of rustot.
//!
//! ## Key Buffer Pattern
//!
//! All generated code uses a shared `key_buf: &mut heapless::String<KEY_LEN>` passed through
//! the entire call chain. Each level appends its field segment, does the KV operation, then
//! truncates back. This avoids allocating a new `String<KEY_LEN>` per field in the async
//! state machine (~116 bytes saved per field).

use proc_macro2::TokenStream;
use quote::quote;

/// The KV key path suffix used to store enum variant discriminants.
///
/// For an enum stored under prefix `/foo`, the variant name is stored at `/foo/_variant`.
/// This allows loading/persisting the variant independently of the variant's inner data.
pub const VARIANT_KEY_PATH: &str = "/_variant";

// =============================================================================
// Internal helpers for KV fetch/store code generation
// =============================================================================

/// Generates code for fetching from KV using key_buf and handling the result.
///
/// Uses fixed-size `ValueBuf` from the type's `zero_value_buf()` method.
fn kv_fetch_and_handle(
    krate: &TokenStream,
    on_some: impl Fn(TokenStream) -> TokenStream,
    on_none: TokenStream,
) -> TokenStream {
    let on_some_body = on_some(quote! { data });

    quote! {
        {
            let mut __fetch_buf = <Self as #krate::shadows::KVPersist>::zero_value_buf();
            if let Some(data) = kv.fetch(key_buf.as_str(), __fetch_buf.as_mut()).await.map_err(#krate::shadows::KvError::Kv)? {
                #on_some_body
            } else {
                #on_none
            }
        }
    }
}

/// Generates code for serializing with postcard and storing to KV using key_buf.
fn kv_serialize_and_store(
    krate: &TokenStream,
    value_expr: &TokenStream,
) -> TokenStream {
    quote! {
        {
            let mut __ser_buf = <Self as #krate::shadows::KVPersist>::zero_value_buf();
            let bytes = #krate::__macro_support::postcard::to_slice(#value_expr, __ser_buf.as_mut())
                .map_err(|_| #krate::shadows::KvError::Serialization)?;
            kv.store(key_buf.as_str(), bytes).await.map_err(#krate::shadows::KvError::Kv)?;
        }
    }
}

/// Generates code for loading a leaf field from KV storage.
///
/// Uses push/truncate on the shared `key_buf` instead of allocating a new key.
pub fn leaf_load(
    krate: &TokenStream,
    field_path: &str,
    field_access: TokenStream,
    on_missing: TokenStream,
) -> TokenStream {
    let fetch_code = kv_fetch_and_handle(
        krate,
        |data_ref| {
            quote! {
                #field_access = #krate::__macro_support::postcard::from_bytes(#data_ref).map_err(|_| #krate::shadows::KvError::Serialization)?;
                result.loaded += 1;
            }
        },
        on_missing,
    );

    quote! {
        {
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#field_path);
            #fetch_code
            key_buf.truncate(__saved_len);
        }
    }
}

/// Generates code for loading a leaf field with migration support.
///
/// Tries to load from the primary key first, falls back to migration sources if:
/// - The key doesn't exist, or
/// - Deserialization fails (schema changed)
///
/// Uses push/truncate on the shared `key_buf` for both primary and migration keys.
pub fn leaf_load_with_migration(
    krate: &TokenStream,
    field_path: &str,
    field_access: TokenStream,
    migrate_from_keys: &[&String],
    migrate_convert: Option<&syn::Path>,
) -> TokenStream {
    if migrate_from_keys.is_empty() {
        // No migration sources - same as leaf_load with default on missing
        let on_missing = quote! {
            <Self as #krate::shadows::KVPersist>::apply_field_default(self, #field_path);
            result.defaulted += 1;
        };
        let fetch_code = kv_fetch_and_handle(
            krate,
            |data_ref| {
                quote! {
                    match #krate::__macro_support::postcard::from_bytes(#data_ref) {
                        Ok(val) => {
                            #field_access = val;
                            result.loaded += 1;
                        }
                        Err(_) => {
                            <Self as #krate::shadows::KVPersist>::apply_field_default(self, #field_path);
                            result.defaulted += 1;
                        }
                    }
                }
            },
            on_missing,
        );

        return quote! {
            {
                let __saved_len = key_buf.len();
                let _ = key_buf.push_str(#field_path);
                #fetch_code
                key_buf.truncate(__saved_len);
            }
        };
    }

    // Has migration sources - generate migration attempts
    let convert_expr = match migrate_convert {
        Some(convert) => quote! { Some(#convert) },
        None => quote! { None },
    };

    let migration_attempts: Vec<TokenStream> = migrate_from_keys.iter().map(|source_key| {
        quote! {
            if !__migrated {
                key_buf.truncate(__saved_len);
                let _ = key_buf.push_str(#source_key);
                let mut __mig_buf = <Self as #krate::shadows::KVPersist>::zero_value_buf();
                if let Some(old_data) = kv.fetch(key_buf.as_str(), __mig_buf.as_mut()).await.map_err(#krate::shadows::KvError::Kv)? {
                    let __convert_fn: Option<#krate::shadows::migration::ConvertFn> = #convert_expr;
                    let value_bytes = if let Some(convert_fn) = __convert_fn {
                        let mut __conv_buf = <Self as #krate::shadows::KVPersist>::zero_value_buf();
                        let new_len = convert_fn(old_data, __conv_buf.as_mut()).map_err(#krate::shadows::KvError::Migration)?;
                        __mig_buf.as_mut()[..new_len].copy_from_slice(&__conv_buf.as_mut()[..new_len]);
                        &__mig_buf.as_mut()[..new_len]
                    } else {
                        old_data
                    };
                    #field_access = #krate::__macro_support::postcard::from_bytes(value_bytes).map_err(|_| #krate::shadows::KvError::Serialization)?;
                    // Store to primary key
                    key_buf.truncate(__saved_len);
                    let _ = key_buf.push_str(#field_path);
                    kv.store(key_buf.as_str(), value_bytes).await.map_err(#krate::shadows::KvError::Kv)?;
                    result.migrated += 1;
                    __migrated = true;
                }
            }
        }
    }).collect();

    quote! {
        {
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#field_path);

            let mut __primary_loaded = false;
            {
                let mut __fetch_buf = <Self as #krate::shadows::KVPersist>::zero_value_buf();
                if let Some(data) = kv.fetch(key_buf.as_str(), __fetch_buf.as_mut()).await.map_err(#krate::shadows::KvError::Kv)? {
                    match #krate::__macro_support::postcard::from_bytes(data) {
                        Ok(val) => {
                            #field_access = val;
                            result.loaded += 1;
                            __primary_loaded = true;
                        }
                        Err(_) => {}
                    }
                }
            }

            if !__primary_loaded {
                let mut __migrated = false;
                #(#migration_attempts)*
                if !__migrated {
                    <Self as #krate::shadows::KVPersist>::apply_field_default(self, #field_path);
                    result.defaulted += 1;
                }
            }

            key_buf.truncate(__saved_len);
        }
    }
}

/// Generates code for persisting a leaf field to KV storage.
///
/// Uses push/truncate on the shared `key_buf`.
pub fn leaf_persist(krate: &TokenStream, field_path: &str, value_expr: TokenStream) -> TokenStream {
    let value_ref = quote! { &#value_expr };
    let store_code = kv_serialize_and_store(krate, &value_ref);

    quote! {
        {
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#field_path);
            #store_code
            key_buf.truncate(__saved_len);
        }
    }
}

/// Generates code for persisting a delta field (only if Some).
///
/// Uses push/truncate on the shared `key_buf`.
pub fn leaf_persist_delta(
    krate: &TokenStream,
    field_path: &str,
    field_name: &syn::Ident,
) -> TokenStream {
    let value_ref = quote! { val };
    let store_code = kv_serialize_and_store(krate, &value_ref);

    quote! {
        if let Some(ref val) = delta.#field_name {
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#field_path);
            #store_code
            key_buf.truncate(__saved_len);
        }
    }
}

/// Generates code for checking if a relative key matches a leaf field path.
pub fn leaf_is_valid_key(field_path: &str) -> TokenStream {
    quote! {
        rel_key == #field_path
    }
}

/// Generates a FIELD_COUNT expression for a leaf field.
pub fn leaf_field_count() -> TokenStream {
    quote! { 1 }
}

/// Generates code for loading a nested ShadowNode field from KV storage.
///
/// Uses push/truncate on the shared `key_buf`.
pub fn nested_load(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
    field_access: TokenStream,
) -> TokenStream {
    quote! {
        {
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#field_path);
            let inner = <#field_ty as #krate::shadows::KVPersist>::load_from_kv::<K, KEY_LEN>(#field_access, key_buf, kv).await?;
            result.merge(inner);
            key_buf.truncate(__saved_len);
        }
    }
}

/// Generates code for loading a nested ShadowNode field with migration support.
///
/// Uses push/truncate on the shared `key_buf`.
pub fn nested_load_with_migration(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
    field_access: TokenStream,
) -> TokenStream {
    quote! {
        {
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#field_path);
            let inner = <#field_ty as #krate::shadows::KVPersist>::load_from_kv_with_migration::<K, KEY_LEN>(#field_access, key_buf, kv).await?;
            result.merge(inner);
            key_buf.truncate(__saved_len);
        }
    }
}

/// Generates code for persisting a nested ShadowNode field to KV storage.
///
/// Uses push/truncate on the shared `key_buf`.
pub fn nested_persist(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
    field_access: TokenStream,
) -> TokenStream {
    quote! {
        {
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#field_path);
            <#field_ty as #krate::shadows::KVPersist>::persist_to_kv::<K, KEY_LEN>(#field_access, key_buf, kv).await?;
            key_buf.truncate(__saved_len);
        }
    }
}

/// Generates code for persisting a nested delta field.
///
/// Uses push/truncate on the shared `key_buf`.
pub fn nested_persist_delta(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
    field_name: &syn::Ident,
) -> TokenStream {
    quote! {
        if let Some(ref inner_delta) = delta.#field_name {
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#field_path);
            <#field_ty as #krate::shadows::KVPersist>::persist_delta::<K, KEY_LEN>(inner_delta, kv, key_buf).await?;
            key_buf.truncate(__saved_len);
        }
    }
}

/// Generates code for checking if a relative key matches a nested ShadowNode field.
pub fn nested_is_valid_key(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
) -> TokenStream {
    quote! {
        rel_key.strip_prefix(#field_path).map_or(false, |rest| {
            <#field_ty as #krate::shadows::KVPersist>::is_valid_key(rest)
        })
    }
}

/// Generates code for checking if a relative key belongs to a nested field's dynamic collection.
pub fn nested_is_valid_prefix(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
) -> TokenStream {
    quote! {
        rel_key.strip_prefix(#field_path).map_or(false, |rest| {
            <#field_ty as #krate::shadows::KVPersist>::is_valid_key(rest)
                || <#field_ty as #krate::shadows::KVPersist>::is_valid_prefix(rest)
        })
    }
}

/// Generates a FIELD_COUNT expression for a nested field.
pub fn nested_field_count(
    krate: &TokenStream,
    field_ty: &syn::Type,
) -> TokenStream {
    quote! { <#field_ty as #krate::shadows::KVPersist>::FIELD_COUNT }
}

// =============================================================================
// Enum variant KV helpers
// =============================================================================

/// Generates a match arm for loading a newtype enum variant from KV storage.
///
/// Uses push/truncate on the shared `key_buf`.
pub fn enum_variant_load_arm(
    krate: &TokenStream,
    variant_path: &str,
    serde_name: &str,
    variant_ident: &syn::Ident,
    inner_ty: &syn::Type,
) -> TokenStream {
    quote! {
        #serde_name => {
            *self = Self::#variant_ident(Default::default());
            if let Self::#variant_ident(ref mut inner) = self {
                let __saved_len = key_buf.len();
                let _ = key_buf.push_str(#variant_path);
                let inner_result = <#inner_ty as #krate::shadows::KVPersist>::load_from_kv::<K, KEY_LEN>(inner, key_buf, kv).await?;
                result.merge(inner_result);
                key_buf.truncate(__saved_len);
            }
        }
    }
}

/// Generates a match arm for persisting a newtype enum variant to KV storage.
///
/// Uses push/truncate on the shared `key_buf`.
pub fn enum_variant_persist_arm(
    krate: &TokenStream,
    variant_path: &str,
    variant_ident: &syn::Ident,
    inner_ty: &syn::Type,
) -> TokenStream {
    quote! {
        Self::#variant_ident(ref inner) => {
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#variant_path);
            <#inner_ty as #krate::shadows::KVPersist>::persist_to_kv::<K, KEY_LEN>(inner, key_buf, kv).await?;
            key_buf.truncate(__saved_len);
        }
    }
}

// =============================================================================
// Enum KVPersist method body generators
// =============================================================================

/// Generates the body of `load_from_kv` for enum types.
///
/// Uses push/truncate on the shared `key_buf` for the variant key.
pub fn enum_load_from_kv_body(
    krate: &TokenStream,
    load_variant_arms: &[TokenStream],
) -> TokenStream {
    quote! {
        async move {
            let mut result = #krate::shadows::LoadFieldResult::default();

            // Read _variant key
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#VARIANT_KEY_PATH);

            let mut __vbuf = [0u8; 32];
            let variant_name = match kv.fetch(key_buf.as_str(), &mut __vbuf).await.map_err(#krate::shadows::KvError::Kv)? {
                Some(data) => core::str::from_utf8(data).map_err(|_| #krate::shadows::KvError::InvalidVariant)?,
                None => {
                    key_buf.truncate(__saved_len);
                    *self = Self::default();
                    return Ok(result);
                }
            };
            key_buf.truncate(__saved_len);

            match variant_name {
                #(#load_variant_arms)*
                _ => return Err(#krate::shadows::KvError::UnknownVariant),
            }

            Ok(result)
        }
    }
}

/// Generates the body of `persist_to_kv` for enum types.
///
/// Uses push/truncate on the shared `key_buf` for the variant key.
pub fn enum_persist_to_kv_body(
    krate: &TokenStream,
    variant_name_arms: &[TokenStream],
    persist_variant_arms: &[TokenStream],
) -> TokenStream {
    quote! {
        async move {
            // Write _variant key
            let __saved_len = key_buf.len();
            let _ = key_buf.push_str(#VARIANT_KEY_PATH);

            // Write variant name
            let variant_name: &str = match self {
                #(#variant_name_arms)*
            };
            kv.store(key_buf.as_str(), variant_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;
            key_buf.truncate(__saved_len);

            // Persist inner fields
            match self {
                #(#persist_variant_arms)*
            }

            Ok(())
        }
    }
}

/// Generates the body of `is_valid_key` for enum types.
///
/// This is shared between simple enums and adjacently-tagged enums.
pub fn enum_is_valid_key_body(
    _krate: &TokenStream,
    is_valid_key_arms: &[TokenStream],
) -> TokenStream {
    quote! {
        // Check _variant key
        rel_key == #VARIANT_KEY_PATH
        // Check all variant fields (not just active)
        #(|| #is_valid_key_arms)*
    }
}

/// Generates the body of `is_valid_prefix` for enum types.
pub fn enum_is_valid_prefix_body(
    is_valid_prefix_arms: &[TokenStream],
) -> TokenStream {
    if is_valid_prefix_arms.is_empty() {
        quote! { false }
    } else {
        quote! {
            false #(|| #is_valid_prefix_arms)*
        }
    }
}

/// Generates the FIELD_COUNT expression for enum types.
pub fn enum_field_count_expr(
    field_count_items: &[TokenStream],
) -> TokenStream {
    if field_count_items.is_empty() {
        quote! { 1 } // just _variant
    } else {
        quote! { 1 #(+ #field_count_items)* } // _variant + all variant fields
    }
}
