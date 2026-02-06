//! KV persistence code generation helpers
//!
//! This module provides reusable code generation patterns for KVPersist trait implementations.
//! These helpers eliminate duplication between struct, enum, and adjacently-tagged enum codegen.
//!
//! The generated code always uses fixed-size buffers (`zero_value_buf()` + `kv.fetch()` +
//! `postcard::to_slice()`). Types that need allocating paths (std String, Vec, HashMap) handle
//! that internally in their own KVPersist impls — the codegen never emits `#[cfg(feature)]`
//! blocks, which would be evaluated in the user's crate instead of rustot.

use proc_macro2::TokenStream;
use quote::quote;

/// The KV key path suffix used to store enum variant discriminants.
///
/// For an enum stored under prefix `/foo`, the variant name is stored at `/foo/_variant`.
/// This allows loading/persisting the variant independently of the variant's inner data.
pub const VARIANT_KEY_PATH: &str = "/_variant";

/// Generates code to build a heapless::String key from a prefix and path.
///
/// # Generated code pattern
/// ```ignore
/// let mut #var_name: ::heapless::String<KEY_LEN> = ::heapless::String::new();
/// let _ = #var_name.push_str(prefix);
/// let _ = #var_name.push_str(#path);
/// ```
pub fn build_key(var_name: &syn::Ident, path: &str) -> TokenStream {
    quote! {
        let mut #var_name: ::heapless::String<KEY_LEN> = ::heapless::String::new();
        let _ = #var_name.push_str(prefix);
        let _ = #var_name.push_str(#path);
    }
}

// =============================================================================
// Internal helpers for KV fetch/store code generation
// =============================================================================

/// Generates code for fetching from KV and handling the result.
///
/// Uses fixed-size `ValueBuf` from the type's `zero_value_buf()` method.
fn kv_fetch_and_handle(
    krate: &TokenStream,
    key_expr: &TokenStream,
    on_some: impl Fn(TokenStream) -> TokenStream,
    on_none: TokenStream,
) -> TokenStream {
    let on_some_body = on_some(quote! { data });

    quote! {
        {
            let mut __fetch_buf = <Self as #krate::shadows::KVPersist>::zero_value_buf();
            if let Some(data) = kv.fetch(#key_expr, __fetch_buf.as_mut()).await.map_err(#krate::shadows::KvError::Kv)? {
                #on_some_body
            } else {
                #on_none
            }
        }
    }
}

/// Generates code for serializing with postcard and storing to KV.
fn kv_serialize_and_store(
    krate: &TokenStream,
    key_expr: &TokenStream,
    value_expr: &TokenStream,
) -> TokenStream {
    quote! {
        {
            let mut __ser_buf = <Self as #krate::shadows::KVPersist>::zero_value_buf();
            let bytes = ::postcard::to_slice(#value_expr, __ser_buf.as_mut())
                .map_err(|_| #krate::shadows::KvError::Serialization)?;
            kv.store(#key_expr, bytes).await.map_err(#krate::shadows::KvError::Kv)?;
        }
    }
}

/// Generates code for loading a leaf field from KV storage.
pub fn leaf_load(
    krate: &TokenStream,
    field_path: &str,
    field_access: TokenStream,
    on_missing: TokenStream,
) -> TokenStream {
    let key_ident = syn::Ident::new("full_key", proc_macro2::Span::call_site());
    let key_code = build_key(&key_ident, field_path);
    let key_expr = quote! { &#key_ident };

    let fetch_code = kv_fetch_and_handle(
        krate,
        &key_expr,
        |data_ref| {
            quote! {
                #field_access = ::postcard::from_bytes(#data_ref).map_err(|_| #krate::shadows::KvError::Serialization)?;
                result.loaded += 1;
            }
        },
        on_missing,
    );

    quote! {
        {
            #key_code
            #fetch_code
        }
    }
}

/// Generates code for loading a leaf field with migration support.
///
/// Tries to load from the primary key first, falls back to migration sources if:
/// - The key doesn't exist, or
/// - Deserialization fails (schema changed)
pub fn leaf_load_with_migration(
    krate: &TokenStream,
    field_path: &str,
    field_access: TokenStream,
    migration_code: TokenStream,
) -> TokenStream {
    let key_ident = syn::Ident::new("full_key", proc_macro2::Span::call_site());
    let key_code = build_key(&key_ident, field_path);
    let key_expr = quote! { &#key_ident };

    // Clone migration_code since it's used in both branches
    let migration_code_clone = migration_code.clone();
    let fetch_code = kv_fetch_and_handle(
        krate,
        &key_expr,
        |data_ref| {
            quote! {
                match ::postcard::from_bytes(#data_ref) {
                    Ok(val) => {
                        #field_access = val;
                        result.loaded += 1;
                    }
                    Err(_) => {
                        #migration_code_clone
                    }
                }
            }
        },
        migration_code,
    );

    quote! {
        {
            #key_code
            #fetch_code
        }
    }
}

/// Generates the migration fallback code that tries old keys and applies conversion.
pub fn migration_fallback(
    krate: &TokenStream,
    field_path: &str,
    field_access: TokenStream,
    migrate_from_keys: &[&String],
    migrate_convert: Option<&syn::Path>,
) -> TokenStream {
    if migrate_from_keys.is_empty() {
        // No migration sources, just apply default
        return quote! {
            <Self as #krate::shadows::KVPersist>::apply_field_default(self, #field_path);
            result.defaulted += 1;
        };
    }

    let convert_expr = match migrate_convert {
        Some(convert) => quote! { Some(#convert) },
        None => quote! { None },
    };

    quote! {
        // Try migration sources
        let mut migrated = false;
        let sources: &[#krate::shadows::MigrationSource] = &[
            #(#krate::shadows::MigrationSource {
                key: #migrate_from_keys,
                convert: #convert_expr,
            }),*
        ];
        for source in sources {
            let mut old_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
            let _ = old_key.push_str(prefix);
            let _ = old_key.push_str(source.key);

            let mut fetch_buf = <Self as #krate::shadows::KVPersist>::zero_value_buf();
            if let Some(old_data) = kv.fetch(&old_key, fetch_buf.as_mut()).await.map_err(#krate::shadows::KvError::Kv)? {
                let value_bytes = if let Some(convert_fn) = source.convert {
                    let mut convert_buf = <Self as #krate::shadows::KVPersist>::zero_value_buf();
                    let new_len = convert_fn(old_data, convert_buf.as_mut()).map_err(#krate::shadows::KvError::Migration)?;
                    fetch_buf.as_mut()[..new_len].copy_from_slice(&convert_buf.as_mut()[..new_len]);
                    &fetch_buf.as_mut()[..new_len]
                } else {
                    old_data
                };
                #field_access = ::postcard::from_bytes(value_bytes).map_err(|_| #krate::shadows::KvError::Serialization)?;
                kv.store(&full_key, value_bytes).await.map_err(#krate::shadows::KvError::Kv)?;
                result.migrated += 1;
                migrated = true;
                break;
            }
        }
        if !migrated {
            <Self as #krate::shadows::KVPersist>::apply_field_default(self, #field_path);
            result.defaulted += 1;
        }
    }
}

/// Generates code for persisting a leaf field to KV storage.
pub fn leaf_persist(krate: &TokenStream, field_path: &str, value_expr: TokenStream) -> TokenStream {
    let key_ident = syn::Ident::new("full_key", proc_macro2::Span::call_site());
    let key_code = build_key(&key_ident, field_path);
    let key_expr = quote! { &#key_ident };
    let value_ref = quote! { &#value_expr };
    let store_code = kv_serialize_and_store(krate, &key_expr, &value_ref);

    quote! {
        {
            #key_code
            #store_code
        }
    }
}

/// Generates code for persisting a delta field (only if Some).
pub fn leaf_persist_delta(
    krate: &TokenStream,
    field_path: &str,
    field_name: &syn::Ident,
) -> TokenStream {
    let key_ident = syn::Ident::new("full_key", proc_macro2::Span::call_site());
    let key_code = build_key(&key_ident, field_path);
    let key_expr = quote! { &#key_ident };
    let value_ref = quote! { val };
    let store_code = kv_serialize_and_store(krate, &key_expr, &value_ref);

    quote! {
        if let Some(ref val) = delta.#field_name {
            #key_code
            #store_code
        }
    }
}

/// Generates code for collecting a leaf field's key.
pub fn leaf_collect_keys(field_path: &str) -> TokenStream {
    let key_ident = syn::Ident::new("key", proc_macro2::Span::call_site());
    let key_code = build_key(&key_ident, field_path);

    quote! {
        {
            #key_code
            keys(&#key_ident);
        }
    }
}

/// Generates code for loading a nested ShadowNode field from KV storage.
pub fn nested_load(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
    field_access: TokenStream,
) -> TokenStream {
    let prefix_ident = syn::Ident::new("nested_prefix", proc_macro2::Span::call_site());
    let prefix_code = build_key(&prefix_ident, field_path);

    quote! {
        {
            #prefix_code
            let inner = <#field_ty as #krate::shadows::KVPersist>::load_from_kv::<K, KEY_LEN>(#field_access, &#prefix_ident, kv).await?;
            result.merge(inner);
        }
    }
}

/// Generates code for loading a nested ShadowNode field with migration support.
pub fn nested_load_with_migration(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
    field_access: TokenStream,
) -> TokenStream {
    let prefix_ident = syn::Ident::new("nested_prefix", proc_macro2::Span::call_site());
    let prefix_code = build_key(&prefix_ident, field_path);

    quote! {
        {
            #prefix_code
            let inner = <#field_ty as #krate::shadows::KVPersist>::load_from_kv_with_migration::<K, KEY_LEN>(#field_access, &#prefix_ident, kv).await?;
            result.merge(inner);
        }
    }
}

/// Generates code for persisting a nested ShadowNode field to KV storage.
pub fn nested_persist(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
    field_access: TokenStream,
) -> TokenStream {
    let prefix_ident = syn::Ident::new("nested_prefix", proc_macro2::Span::call_site());
    let prefix_code = build_key(&prefix_ident, field_path);

    quote! {
        {
            #prefix_code
            <#field_ty as #krate::shadows::KVPersist>::persist_to_kv::<K, KEY_LEN>(#field_access, &#prefix_ident, kv).await?;
        }
    }
}

/// Generates code for persisting a nested delta field.
pub fn nested_persist_delta(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
    field_name: &syn::Ident,
) -> TokenStream {
    let prefix_ident = syn::Ident::new("nested_prefix", proc_macro2::Span::call_site());
    let prefix_code = build_key(&prefix_ident, field_path);

    quote! {
        if let Some(ref inner_delta) = delta.#field_name {
            #prefix_code
            <#field_ty as #krate::shadows::KVPersist>::persist_delta::<K, KEY_LEN>(inner_delta, kv, &#prefix_ident).await?;
        }
    }
}

/// Generates code for collecting keys from a nested ShadowNode field.
pub fn nested_collect_keys(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
) -> TokenStream {
    let prefix_ident = syn::Ident::new("nested_prefix", proc_macro2::Span::call_site());
    let prefix_code = build_key(&prefix_ident, field_path);

    quote! {
        {
            #prefix_code
            <#field_ty as #krate::shadows::KVPersist>::collect_valid_keys::<KEY_LEN>(&#prefix_ident, keys);
        }
    }
}

/// Generates code for collecting prefixes from a nested ShadowNode field.
pub fn nested_collect_prefixes(
    krate: &TokenStream,
    field_path: &str,
    field_ty: &syn::Type,
) -> TokenStream {
    let prefix_ident = syn::Ident::new("nested_prefix", proc_macro2::Span::call_site());
    let prefix_code = build_key(&prefix_ident, field_path);

    quote! {
        {
            #prefix_code
            <#field_ty as #krate::shadows::KVPersist>::collect_valid_prefixes::<KEY_LEN>(&#prefix_ident, prefixes);
        }
    }
}

// =============================================================================
// Enum variant KV helpers
// =============================================================================

/// Generates a match arm for loading a newtype enum variant from KV storage.
///
/// This handles the pattern of:
/// 1. Setting self to the variant with a default inner value
/// 2. Delegating to the inner type's load_from_kv
pub fn enum_variant_load_arm(
    krate: &TokenStream,
    variant_path: &str,
    serde_name: &str,
    variant_ident: &syn::Ident,
    inner_ty: &syn::Type,
) -> TokenStream {
    let prefix_ident = syn::Ident::new("inner_prefix", proc_macro2::Span::call_site());
    let prefix_code = build_key(&prefix_ident, variant_path);

    quote! {
        #serde_name => {
            *self = Self::#variant_ident(Default::default());
            if let Self::#variant_ident(ref mut inner) = self {
                #prefix_code
                let inner_result = <#inner_ty as #krate::shadows::KVPersist>::load_from_kv::<K, KEY_LEN>(inner, &#prefix_ident, kv).await?;
                result.merge(inner_result);
            }
        }
    }
}

/// Generates a match arm for persisting a newtype enum variant to KV storage.
pub fn enum_variant_persist_arm(
    krate: &TokenStream,
    variant_path: &str,
    variant_ident: &syn::Ident,
    inner_ty: &syn::Type,
) -> TokenStream {
    let prefix_ident = syn::Ident::new("inner_prefix", proc_macro2::Span::call_site());
    let prefix_code = build_key(&prefix_ident, variant_path);

    quote! {
        Self::#variant_ident(ref inner) => {
            #prefix_code
            <#inner_ty as #krate::shadows::KVPersist>::persist_to_kv::<K, KEY_LEN>(inner, &#prefix_ident, kv).await?;
        }
    }
}

// =============================================================================
// Enum KVPersist method body generators
// =============================================================================

/// Generates the body of `load_from_kv` for enum types.
///
/// This is shared between simple enums and adjacently-tagged enums.
pub fn enum_load_from_kv_body(
    krate: &TokenStream,
    load_variant_arms: &[TokenStream],
) -> TokenStream {
    let variant_key_ident = syn::Ident::new("variant_key", proc_macro2::Span::call_site());
    let variant_key_code = build_key(&variant_key_ident, VARIANT_KEY_PATH);

    quote! {
        async move {
            let mut result = #krate::shadows::LoadFieldResult::default();

            // Read _variant key (variant names are short, 128 bytes is plenty)
            #variant_key_code

            let mut __vbuf = [0u8; 128];
            let variant_name = match kv.fetch(&#variant_key_ident, &mut __vbuf).await.map_err(#krate::shadows::KvError::Kv)? {
                Some(data) => core::str::from_utf8(data).map_err(|_| #krate::shadows::KvError::InvalidVariant)?,
                None => {
                    *self = Self::default();
                    return Ok(result);
                }
            };

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
/// This is shared between simple enums and adjacently-tagged enums.
pub fn enum_persist_to_kv_body(
    krate: &TokenStream,
    variant_name_arms: &[TokenStream],
    persist_variant_arms: &[TokenStream],
) -> TokenStream {
    let variant_key_ident = syn::Ident::new("variant_key", proc_macro2::Span::call_site());
    let variant_key_code = build_key(&variant_key_ident, VARIANT_KEY_PATH);

    quote! {
        async move {
            // Write _variant key
            #variant_key_code

            // Write variant name
            let variant_name: &str = match self {
                #(#variant_name_arms)*
            };
            kv.store(&#variant_key_ident, variant_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;

            // Persist inner fields
            match self {
                #(#persist_variant_arms)*
            }

            Ok(())
        }
    }
}

/// Generates the body of `collect_valid_keys` for enum types.
///
/// This is shared between simple enums and adjacently-tagged enums.
pub fn enum_collect_valid_keys_body(collect_keys_arms: &[TokenStream]) -> TokenStream {
    let variant_key_ident = syn::Ident::new("variant_key", proc_macro2::Span::call_site());
    let variant_key_code = build_key(&variant_key_ident, VARIANT_KEY_PATH);

    quote! {
        // Add _variant key
        #variant_key_code
        keys(&#variant_key_ident);

        // Collect from all variants (not just active)
        #(#collect_keys_arms)*
    }
}
