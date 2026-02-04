//! KV persistence code generation helpers
//!
//! This module provides reusable code generation patterns for KVPersist trait implementations.
//! These helpers eliminate duplication between struct, enum, and adjacently-tagged enum codegen.

use proc_macro2::TokenStream;
use quote::quote;

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

/// Generates code for loading a leaf field from KV storage.
///
/// Handles both std and no_std environments with appropriate buffer management.
pub fn leaf_load(
    krate: &TokenStream,
    field_path: &str,
    field_access: TokenStream,
    on_missing: TokenStream,
) -> TokenStream {
    let key_ident = syn::Ident::new("full_key", proc_macro2::Span::call_site());
    let key_code = build_key(&key_ident, field_path);

    quote! {
        {
            #key_code
            #[cfg(not(feature = "std"))]
            {
                let mut __fetch_buf = [0u8; <Self as #krate::shadows::KVPersist>::MAX_VALUE_LEN];
                if let Some(data) = kv.fetch(&#key_ident, &mut __fetch_buf).await.map_err(#krate::shadows::KvError::Kv)? {
                    #field_access = ::postcard::from_bytes(data).map_err(|_| #krate::shadows::KvError::Serialization)?;
                    result.loaded += 1;
                } else {
                    #on_missing
                }
            }
            #[cfg(feature = "std")]
            {
                if let Some(data) = kv.fetch_to_vec(&#key_ident).await.map_err(#krate::shadows::KvError::Kv)? {
                    #field_access = ::postcard::from_bytes(&data).map_err(|_| #krate::shadows::KvError::Serialization)?;
                    result.loaded += 1;
                } else {
                    #on_missing
                }
            }
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

    quote! {
        {
            #key_code
            #[cfg(not(feature = "std"))]
            {
                let mut __fetch_buf = [0u8; <Self as #krate::shadows::KVPersist>::MAX_VALUE_LEN];
                if let Some(data) = kv.fetch(&#key_ident, &mut __fetch_buf).await.map_err(#krate::shadows::KvError::Kv)? {
                    match ::postcard::from_bytes(data) {
                        Ok(val) => {
                            #field_access = val;
                            result.loaded += 1;
                        }
                        Err(_) => {
                            #migration_code
                        }
                    }
                } else {
                    #migration_code
                }
            }
            #[cfg(feature = "std")]
            {
                if let Some(data) = kv.fetch_to_vec(&#key_ident).await.map_err(#krate::shadows::KvError::Kv)? {
                    match ::postcard::from_bytes(&data) {
                        Ok(val) => {
                            #field_access = val;
                            result.loaded += 1;
                        }
                        Err(_) => {
                            #migration_code
                        }
                    }
                } else {
                    #migration_code
                }
            }
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
            #[cfg(not(feature = "std"))]
            {
                let mut fetch_buf = [0u8; <Self as #krate::shadows::KVPersist>::MAX_VALUE_LEN];
                if let Some(old_data) = kv.fetch(&old_key, &mut fetch_buf).await.map_err(#krate::shadows::KvError::Kv)? {
                    let value_bytes = if let Some(convert_fn) = source.convert {
                        let mut convert_buf = [0u8; <Self as #krate::shadows::KVPersist>::MAX_VALUE_LEN];
                        let new_len = convert_fn(old_data, &mut convert_buf).map_err(#krate::shadows::KvError::Migration)?;
                        fetch_buf[..new_len].copy_from_slice(&convert_buf[..new_len]);
                        &fetch_buf[..new_len]
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
            #[cfg(feature = "std")]
            {
                if let Some(old_data) = kv.fetch_to_vec(&old_key).await.map_err(#krate::shadows::KvError::Kv)? {
                    let value_bytes: ::std::vec::Vec<u8> = if let Some(convert_fn) = source.convert {
                        let mut convert_buf = vec![0u8; old_data.len() * 2 + 64];
                        let new_len = convert_fn(&old_data, &mut convert_buf).map_err(#krate::shadows::KvError::Migration)?;
                        convert_buf[..new_len].to_vec()
                    } else {
                        old_data
                    };
                    #field_access = ::postcard::from_bytes(&value_bytes).map_err(|_| #krate::shadows::KvError::Serialization)?;
                    kv.store(&full_key, &value_bytes).await.map_err(#krate::shadows::KvError::Kv)?;
                    result.migrated += 1;
                    migrated = true;
                    break;
                }
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

    quote! {
        {
            #key_code
            #[cfg(not(feature = "std"))]
            {
                let mut __ser_buf = [0u8; <Self as #krate::shadows::KVPersist>::MAX_VALUE_LEN];
                let bytes = ::postcard::to_slice(&#value_expr, &mut __ser_buf)
                    .map_err(|_| #krate::shadows::KvError::Serialization)?;
                kv.store(&#key_ident, bytes).await.map_err(#krate::shadows::KvError::Kv)?;
            }
            #[cfg(feature = "std")]
            {
                let bytes = ::postcard::to_allocvec(&#value_expr)
                    .map_err(|_| #krate::shadows::KvError::Serialization)?;
                kv.store(&#key_ident, &bytes).await.map_err(#krate::shadows::KvError::Kv)?;
            }
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

    quote! {
        if let Some(ref val) = delta.#field_name {
            #key_code
            #[cfg(not(feature = "std"))]
            {
                let mut __ser_buf = [0u8; <Self as #krate::shadows::KVPersist>::MAX_VALUE_LEN];
                let bytes = ::postcard::to_slice(val, &mut __ser_buf)
                    .map_err(|_| #krate::shadows::KvError::Serialization)?;
                kv.store(&#key_ident, bytes).await.map_err(#krate::shadows::KvError::Kv)?;
            }
            #[cfg(feature = "std")]
            {
                let bytes = ::postcard::to_allocvec(val)
                    .map_err(|_| #krate::shadows::KvError::Serialization)?;
                kv.store(&#key_ident, &bytes).await.map_err(#krate::shadows::KvError::Kv)?;
            }
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
pub fn nested_collect_keys(krate: &TokenStream, field_path: &str, field_ty: &syn::Type) -> TokenStream {
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

/// Generates a match arm for persisting a newtype delta variant's inner data.
///
/// This is used for the config/inner data delegation, not the variant key write.
pub fn enum_variant_persist_delta_inner_arm(
    krate: &TokenStream,
    variant_path: &str,
    delta_variant_path: TokenStream,
    inner_ty: &syn::Type,
) -> TokenStream {
    let prefix_ident = syn::Ident::new("inner_prefix", proc_macro2::Span::call_site());
    let prefix_code = build_key(&prefix_ident, variant_path);

    quote! {
        #delta_variant_path(ref inner_delta) => {
            #prefix_code
            <#inner_ty as #krate::shadows::KVPersist>::persist_delta::<K, KEY_LEN>(inner_delta, kv, &#prefix_ident).await?;
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
    let variant_key_code = build_key(&variant_key_ident, "/_variant");

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
    let variant_key_code = build_key(&variant_key_ident, "/_variant");

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
    let variant_key_code = build_key(&variant_key_ident, "/_variant");

    quote! {
        // Add _variant key
        #variant_key_code
        keys(&#variant_key_ident);

        // Collect from all variants (not just active)
        #(#collect_keys_arms)*
    }
}
