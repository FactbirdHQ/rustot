//! Code generation for adjacently-tagged enum types
//!
//! For an enum like:
//! ```ignore
//! #[shadow_node]
//! #[serde(tag = "mode", content = "config", rename_all = "lowercase")]
//! pub enum PortMode {
//!     #[default]
//!     Inactive,
//!     Sio(SioConfig),
//! }
//! ```
//!
//! Generates:
//! - `PortModeVariant { Inactive, Sio }` - discriminant enum
//! - `DeltaPortModeConfig { Sio(DeltaSioConfig) }` - config delta enum (variants with data only)
//! - `DeltaPortMode { mode: Option<...>, config: Option<...> }` - struct delta
//! - `ReportedPortMode { Inactive, Sio(...) }` - reported enum with custom Serialize

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Ident};

use crate::attr::{
    get_serde_rename_all, get_serde_tag_content, get_variant_serde_name, has_default_attr,
};

/// Generate code for an adjacently-tagged enum type
pub(crate) fn generate_adjacently_tagged_enum_code(
    input: &DeriveInput,
    delta_name: &Ident,
    reported_name: &Ident,
    krate: &TokenStream,
) -> syn::Result<(TokenStream, TokenStream, TokenStream, TokenStream)> {
    let name = &input.ident;
    let vis = &input.vis;

    let variants = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => return Err(syn::Error::new_spanned(input, "expected enum")),
    };

    // Get serde attributes
    let (tag_key, content_key) = get_serde_tag_content(&input.attrs);
    let tag_key = tag_key.ok_or_else(|| {
        syn::Error::new_spanned(
            input,
            "adjacently-tagged enum requires #[serde(tag = \"...\")]",
        )
    })?;
    let content_key = content_key.ok_or_else(|| {
        syn::Error::new_spanned(
            input,
            "adjacently-tagged enum requires #[serde(content = \"...\")]",
        )
    })?;
    let rename_all = get_serde_rename_all(&input.attrs);

    // Find default variant
    let default_variant = variants.iter().find(|v| has_default_attr(&v.attrs));

    // Type names for generated types
    let variant_enum_name = format_ident!("{}Variant", name);
    let delta_config_name = format_ident!("{}Config", delta_name);

    // Collect variant info
    let mut variant_enum_variants = Vec::new(); // For PortModeVariant
    let mut delta_config_variants = Vec::new(); // For DeltaPortModeConfig (only variants with data)
    let mut reported_variants = Vec::new(); // For ReportedPortMode
    let mut variant_names = Vec::new(); // Serde names for all variants
    let mut variant_idents = Vec::new(); // Rust idents for all variants

    // For apply_delta - mode switching
    let mut mode_switch_arms = Vec::new();

    // For apply_delta - config application
    let mut config_apply_arms = Vec::new();

    // For into_reported
    let mut into_reported_arms = Vec::new();

    // For variant name matching
    let mut variant_name_arms = Vec::new();

    // For schema hash
    let mut schema_hash_code = Vec::new();

    // For MAX_VALUE_LEN
    let mut max_value_len_items = Vec::new();

    // For custom Serialize - flat union
    let mut inactive_variant_field_nulls = Vec::new(); // field names to null when not active

    // Track which variants have data (for DeltaConfig enum)
    let mut variants_with_data: Vec<(&syn::Variant, String)> = Vec::new();

    // KVPersist-specific codegen (feature-gated)
    let mut load_from_kv_variant_arms = Vec::new();
    let mut persist_to_kv_variant_arms = Vec::new();
    let mut persist_delta_mode_arms = Vec::new();
    let mut persist_delta_config_arms = Vec::new();
    let mut collect_valid_keys_arms = Vec::new();
    let mut collect_valid_prefixes_arms = Vec::new();
    let mut max_key_len_items = Vec::new();

    // "_variant" key path length
    let variant_key_len = "/_variant".len();

    for variant in variants {
        let variant_ident = &variant.ident;
        let serde_name = get_variant_serde_name(variant, rename_all.as_deref());

        variant_names.push(serde_name.clone());
        variant_idents.push(variant_ident.clone());

        // Add to variant discriminant enum
        variant_enum_variants.push(quote! { #variant_ident, });

        match &variant.fields {
            Fields::Unit => {
                // Unit variant - no data
                reported_variants.push(quote! { #variant_ident, });

                variant_name_arms.push(quote! {
                    Self::#variant_ident => #serde_name,
                });

                mode_switch_arms.push(quote! {
                    #variant_enum_name::#variant_ident => {
                        *self = Self::#variant_ident;
                    }
                });

                into_reported_arms.push(quote! {
                    Self::#variant_ident => Self::Reported::#variant_ident,
                });

                // Schema hash for unit variant
                let variant_bytes = serde_name.as_bytes();
                schema_hash_code.push(quote! {
                    h = #krate::shadows::fnv1a_bytes(h, &[#(#variant_bytes),*]);
                });

                // =====================================================================
                // KVPersist codegen for unit variant
                // =====================================================================

                // load_from_kv arm: just set the variant
                load_from_kv_variant_arms.push(quote! {
                    #serde_name => {
                        *self = Self::#variant_ident;
                    }
                });

                // persist_to_kv arm: unit variants have no inner fields to persist
                persist_to_kv_variant_arms.push(quote! {
                    Self::#variant_ident => {
                        // No inner fields to persist
                    }
                });

                // persist_delta mode arm: write _variant key
                persist_delta_mode_arms.push(quote! {
                    #variant_enum_name::#variant_ident => {
                        kv.store(&variant_key, #serde_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;
                    }
                });
            }
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                // Newtype variant - has data
                let inner_ty = &fields.unnamed[0].ty;

                variants_with_data.push((variant, serde_name.clone()));

                // Nested ShadowNode
                let delta_inner_ty = quote! { <#inner_ty as #krate::shadows::ShadowNode>::Delta };
                let reported_inner_ty =
                    quote! { <#inner_ty as #krate::shadows::ShadowNode>::Reported };

                delta_config_variants.push(quote! { #variant_ident(#delta_inner_ty), });
                reported_variants.push(quote! { #variant_ident(#reported_inner_ty), });

                variant_name_arms.push(quote! {
                    Self::#variant_ident(_) => #serde_name,
                });

                mode_switch_arms.push(quote! {
                    #variant_enum_name::#variant_ident => {
                        *self = Self::#variant_ident(Default::default());
                    }
                });

                config_apply_arms.push(quote! {
                    (Self::#variant_ident(ref mut inner), #delta_config_name::#variant_ident(ref delta_inner)) => {
                        inner.apply_delta(delta_inner);
                    }
                });

                into_reported_arms.push(quote! {
                    Self::#variant_ident(inner) => Self::Reported::#variant_ident(inner.into_reported()),
                });

                max_value_len_items.push(quote! {
                    <#inner_ty as #krate::shadows::KVPersist>::MAX_VALUE_LEN
                });

                // For flat union serialize - use ReportedUnionFields
                inactive_variant_field_nulls.push((variant_ident.clone(), inner_ty.clone()));

                // Schema hash for newtype variant
                let variant_bytes = serde_name.as_bytes();
                schema_hash_code.push(quote! {
                    h = #krate::shadows::fnv1a_bytes(h, &[#(#variant_bytes),*]);
                    h = #krate::shadows::fnv1a_u64(h, <#inner_ty as #krate::shadows::ShadowNode>::SCHEMA_HASH);
                });

                // =====================================================================
                // KVPersist codegen for newtype variant
                // =====================================================================
                let variant_path = format!("/{}", serde_name);
                let variant_path_len = variant_path.len();

                // MAX_KEY_LEN: "/VariantName" + nested MAX_KEY_LEN
                max_key_len_items.push(quote! { #variant_path_len + <#inner_ty as #krate::shadows::KVPersist>::MAX_KEY_LEN });

                // load_from_kv arm: construct variant, delegate to inner
                load_from_kv_variant_arms.push(quote! {
                    #serde_name => {
                        *self = Self::#variant_ident(Default::default());
                        if let Self::#variant_ident(ref mut inner) = self {
                            let mut inner_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                            let _ = inner_prefix.push_str(prefix);
                            let _ = inner_prefix.push_str(#variant_path);
                            let inner_result = <#inner_ty as #krate::shadows::KVPersist>::load_from_kv::<K, KEY_LEN>(inner, &inner_prefix, kv).await?;
                            result.merge(inner_result);
                        }
                    }
                });

                // persist_to_kv arm: delegate to inner
                persist_to_kv_variant_arms.push(quote! {
                    Self::#variant_ident(ref inner) => {
                        let mut inner_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                        let _ = inner_prefix.push_str(prefix);
                        let _ = inner_prefix.push_str(#variant_path);
                        <#inner_ty as #krate::shadows::KVPersist>::persist_to_kv::<K, KEY_LEN>(inner, &inner_prefix, kv).await?;
                    }
                });

                // persist_delta mode arm: write _variant key
                persist_delta_mode_arms.push(quote! {
                    #variant_enum_name::#variant_ident => {
                        kv.store(&variant_key, #serde_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;
                    }
                });

                // persist_delta config arm: delegate to inner
                persist_delta_config_arms.push(quote! {
                    #delta_config_name::#variant_ident(ref inner_delta) => {
                        let mut inner_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                        let _ = inner_prefix.push_str(prefix);
                        let _ = inner_prefix.push_str(#variant_path);
                        <#inner_ty as #krate::shadows::KVPersist>::persist_delta::<K, KEY_LEN>(inner_delta, kv, &inner_prefix).await?;
                    }
                });

                // collect_valid_keys: delegate to inner (all variants, not just active)
                collect_valid_keys_arms.push(quote! {
                    {
                        let mut inner_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                        let _ = inner_prefix.push_str(prefix);
                        let _ = inner_prefix.push_str(#variant_path);
                        <#inner_ty as #krate::shadows::KVPersist>::collect_valid_keys::<KEY_LEN>(&inner_prefix, keys);
                    }
                });

                // collect_valid_prefixes: delegate to inner
                collect_valid_prefixes_arms.push(quote! {
                    {
                        let mut inner_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                        let _ = inner_prefix.push_str(prefix);
                        let _ = inner_prefix.push_str(#variant_path);
                        <#inner_ty as #krate::shadows::KVPersist>::collect_valid_prefixes::<KEY_LEN>(&inner_prefix, prefixes);
                    }
                });
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    variant,
                    "only unit and newtype enum variants are supported",
                ));
            }
        }
    }

    // Build max_value_len const expression
    let max_value_len_expr = if max_value_len_items.is_empty() {
        quote! { 0 }
    } else {
        let mut expr = max_value_len_items[0].clone();
        for item in &max_value_len_items[1..] {
            expr = quote! { const_max(#expr, #item) };
        }
        quote! {
            {
                const fn const_max(a: usize, b: usize) -> usize {
                    if a > b { a } else { b }
                }
                #expr
            }
        }
    };

    // Build MAX_KEY_LEN const expression (max of _variant key and variant paths)
    let max_key_len_expr = if max_key_len_items.is_empty() {
        // Just the _variant key
        quote! { #variant_key_len }
    } else {
        let mut expr = max_key_len_items[0].clone();
        for item in &max_key_len_items[1..] {
            expr = quote! { const_max(#expr, #item) };
        }
        quote! {
            {
                const fn const_max(a: usize, b: usize) -> usize {
                    if a > b { a } else { b }
                }
                const_max(#variant_key_len, #expr)
            }
        }
    };

    // Build SCHEMA_HASH const
    let schema_hash_const = quote! {
        {
            let mut h = #krate::shadows::FNV1A_INIT;
            #(#schema_hash_code)*
            h
        }
    };

    // Generate the serde rename_all attribute if present
    let rename_all_attr = rename_all.as_ref().map(|ra| {
        quote! { #[serde(rename_all = #ra)] }
    });

    // =========================================================================
    // 1. Generate Variant Discriminant Enum
    // =========================================================================
    let variant_default_impl = if let Some(default_var) = default_variant {
        let default_var_ident = &default_var.ident;
        quote! {
            impl Default for #variant_enum_name {
                fn default() -> Self {
                    Self::#default_var_ident
                }
            }
        }
    } else {
        TokenStream::new()
    };

    let variant_enum_type = quote! {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        #rename_all_attr
        #vis enum #variant_enum_name {
            #(#variant_enum_variants)*
        }

        #variant_default_impl
    };

    // =========================================================================
    // 2. Generate Delta Config Enum (only variants with data)
    // =========================================================================
    let delta_config_type = if delta_config_variants.is_empty() {
        // No variants with data - don't generate the config enum
        TokenStream::new()
    } else {
        quote! {
            #[derive(Clone, ::serde::Serialize, ::serde::Deserialize)]
            #rename_all_attr
            #vis enum #delta_config_name {
                #(#delta_config_variants)*
            }
        }
    };

    // =========================================================================
    // 3. Generate Struct-Shaped Delta
    // =========================================================================
    let config_field_for_struct = if delta_config_variants.is_empty() {
        TokenStream::new()
    } else {
        quote! {
            #[serde(rename = #content_key, skip_serializing_if = "Option::is_none")]
            pub config: Option<#delta_config_name>,
        }
    };

    // Generate Delta type
    let delta_type = quote! {
        #variant_enum_type

        #delta_config_type

        #[derive(Clone, Default, ::serde::Serialize, ::serde::Deserialize)]
        #vis struct #delta_name {
            #[serde(rename = #tag_key, skip_serializing_if = "Option::is_none")]
            pub mode: Option<#variant_enum_name>,
            #config_field_for_struct
        }
    };

    // =========================================================================
    // 4. Generate Reported Enum with Custom Serialize
    // =========================================================================
    let reported_default_impl = if let Some(default_var) = default_variant {
        let default_var_ident = &default_var.ident;
        match &default_var.fields {
            Fields::Unit => quote! {
                impl Default for #reported_name {
                    fn default() -> Self {
                        Self::#default_var_ident
                    }
                }
            },
            Fields::Unnamed(_) => quote! {
                impl Default for #reported_name {
                    fn default() -> Self {
                        Self::#default_var_ident(Default::default())
                    }
                }
            },
            _ => TokenStream::new(),
        }
    } else {
        TokenStream::new()
    };

    // Build serialize match arms with null fields for inactive variants integrated
    let combined_serialize_arms: Vec<TokenStream> = variant_idents
        .iter()
        .enumerate()
        .map(|(i, variant_ident)| {
            let serde_name = &variant_names[i];

            // Generate null field serialization for other data variants
            let nulls: Vec<TokenStream> = inactive_variant_field_nulls
                .iter()
                .filter(|(other_ident, _)| {
                    // Only null out fields from OTHER variants
                    other_ident != variant_ident
                })
                .map(|(_, inner_ty)| {
                    quote! {
                        #krate::shadows::serialize_null_fields(
                            <<#inner_ty as #krate::shadows::ShadowNode>::Reported as #krate::shadows::ReportedUnionFields>::FIELD_NAMES,
                            &mut map
                        )?;
                    }
                })
                .collect();

            // Check if this variant has data
            let has_data = variants_with_data
                .iter()
                .any(|(v, _)| v.ident == *variant_ident);

            if has_data {
                // Find the inner type for this variant
                let inner_ty = variants_with_data
                    .iter()
                    .find(|(v, _)| v.ident == *variant_ident)
                    .map(|(v, _)| {
                        if let Fields::Unnamed(fields) = &v.fields {
                            &fields.unnamed[0].ty
                        } else {
                            unreachable!()
                        }
                    })
                    .unwrap();

                quote! {
                    Self::#variant_ident(ref config) => {
                        map.serialize_entry(#tag_key, #serde_name)?;
                        if <<#inner_ty as #krate::shadows::ShadowNode>::Reported
                            as #krate::shadows::ReportedUnionFields>::FIELD_NAMES.is_empty() {
                            map.serialize_entry(#content_key, config)?;
                        } else {
                            config.serialize_into_map(&mut map)?;
                        }
                        #(#nulls)*
                    }
                }
            } else {
                // Unit variant - just serialize mode and nulls
                quote! {
                    Self::#variant_ident => {
                        map.serialize_entry(#tag_key, #serde_name)?;
                        #(#nulls)*
                    }
                }
            }
        })
        .collect();

    // Compute total field count at compile time for serialize_map
    let field_count_arms: Vec<TokenStream> = inactive_variant_field_nulls
        .iter()
        .map(|(_, inner_ty)| {
            quote! {
                <<#inner_ty as #krate::shadows::ShadowNode>::Reported
                    as #krate::shadows::ReportedUnionFields>::FIELD_NAMES.len()
            }
        })
        .collect();

    let total_fields_expr = if field_count_arms.is_empty() {
        quote! { 1usize } // Just the mode field
    } else {
        quote! {
            {
                const fn sum_fields() -> usize {
                    1 // mode field
                    #(+ #field_count_arms)*
                }
                sum_fields()
            }
        }
    };

    let reported_type = quote! {
        #[derive(Clone)]
        #vis enum #reported_name {
            #(#reported_variants)*
        }

        #reported_default_impl

        impl ::serde::Serialize for #reported_name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: ::serde::Serializer,
            {
                use ::serde::ser::SerializeMap;
                use #krate::shadows::ReportedUnionFields;

                const TOTAL_FIELDS: usize = #total_fields_expr;
                let mut map = serializer.serialize_map(Some(TOTAL_FIELDS))?;
                match self {
                    #(#combined_serialize_arms)*
                }
                map.end()
            }
        }
    };

    // =========================================================================
    // 5. Generate ShadowNode Implementation (always available)
    // =========================================================================

    // Config apply handling - if no config variants, no config handling needed
    let config_apply_code = if config_apply_arms.is_empty() {
        TokenStream::new()
    } else {
        quote! {
            if let Some(ref config) = delta.config {
                match (self, config) {
                    #(#config_apply_arms)*
                    _ => { /* Variant mismatch - ignore config if variant doesn't match */ }
                }
            }
        }
    };

    let shadow_node_impl = quote! {
        impl #krate::shadows::ShadowNode for #name {
            type Delta = #delta_name;
            type Reported = #reported_name;

            const SCHEMA_HASH: u64 = #schema_hash_const;

            fn apply_delta(&mut self, delta: &Self::Delta) {
                // Handle mode (variant switch)
                if let Some(ref new_mode) = delta.mode {
                    match new_mode {
                        #(#mode_switch_arms)*
                    }
                }

                // Handle config (variant content)
                #config_apply_code
            }

            fn into_reported(self) -> Self::Reported {
                match self {
                    #(#into_reported_arms)*
                }
            }
        }

        // KVPersist impl (feature-gated)
        #[cfg(feature = "shadows_kv_persist")]
        impl #krate::shadows::KVPersist for #name {
            const MAX_KEY_LEN: usize = #max_key_len_expr;
            const MAX_VALUE_LEN: usize = #max_value_len_expr;

            fn migration_sources(_field_path: &str) -> &'static [#krate::shadows::MigrationSource] {
                &[]
            }

            fn all_migration_keys() -> impl Iterator<Item = &'static str> {
                core::iter::empty()
            }

            fn apply_field_default(&mut self, _field_path: &str) -> bool {
                false
            }

            fn load_from_kv<K: #krate::shadows::KVStore, const KEY_LEN: usize>(
                &mut self,
                prefix: &str,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<#krate::shadows::LoadFieldResult, #krate::shadows::KvError<K::Error>>> {
                async move {
                    let mut result = #krate::shadows::LoadFieldResult::default();

                    // Read _variant key (variant names are short, 128 bytes is plenty)
                    let mut variant_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = variant_key.push_str(prefix);
                    let _ = variant_key.push_str("/_variant");

                    let mut __vbuf = [0u8; 128];
                    let variant_name = match kv.fetch(&variant_key, &mut __vbuf).await.map_err(#krate::shadows::KvError::Kv)? {
                        Some(data) => core::str::from_utf8(data).map_err(|_| #krate::shadows::KvError::InvalidVariant)?,
                        None => {
                            *self = Self::default();
                            return Ok(result);
                        }
                    };

                    match variant_name {
                        #(#load_from_kv_variant_arms)*
                        _ => return Err(#krate::shadows::KvError::UnknownVariant),
                    }

                    Ok(result)
                }
            }

            fn load_from_kv_with_migration<K: #krate::shadows::KVStore, const KEY_LEN: usize>(
                &mut self,
                prefix: &str,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<#krate::shadows::LoadFieldResult, #krate::shadows::KvError<K::Error>>> {
                // Adjacently-tagged enums don't have migration support at this level
                self.load_from_kv::<K, KEY_LEN>(prefix, kv)
            }

            fn persist_to_kv<K: #krate::shadows::KVStore, const KEY_LEN: usize>(
                &self,
                prefix: &str,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<(), #krate::shadows::KvError<K::Error>>> {
                async move {
                    // Write _variant key
                    let mut variant_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = variant_key.push_str(prefix);
                    let _ = variant_key.push_str("/_variant");

                    // Write variant name
                    let variant_name: &str = match self {
                        #(#variant_name_arms)*
                    };
                    kv.store(&variant_key, variant_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;

                    // Persist inner fields
                    match self {
                        #(#persist_to_kv_variant_arms)*
                    }

                    Ok(())
                }
            }

            fn persist_delta<K: #krate::shadows::KVStore, const KEY_LEN: usize>(
                delta: &Self::Delta,
                kv: &K,
                prefix: &str,
            ) -> impl ::core::future::Future<Output = Result<(), #krate::shadows::KvError<K::Error>>> {
                async move {
                    // Build variant key path
                    let mut variant_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = variant_key.push_str(prefix);
                    let _ = variant_key.push_str("/_variant");

                    // Handle mode (variant switch) - only write if mode is Some
                    if let Some(ref new_mode) = delta.mode {
                        match new_mode {
                            #(#persist_delta_mode_arms)*
                        }
                    }

                    // Handle config (variant content)
                    if let Some(ref config) = delta.config {
                        match config {
                            #(#persist_delta_config_arms)*
                        }
                    }

                    Ok(())
                }
            }

            fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
                // Add _variant key
                let mut variant_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                let _ = variant_key.push_str(prefix);
                let _ = variant_key.push_str("/_variant");
                keys(&variant_key);

                // Collect from all variants (not just active)
                #(#collect_valid_keys_arms)*
            }

            fn collect_valid_prefixes<const KEY_LEN: usize>(prefix: &str, prefixes: &mut impl FnMut(&str)) {
                #(#collect_valid_prefixes_arms)*
            }
        }
    };

    // =========================================================================
    // 6. Generate ReportedUnionFields Implementation
    // =========================================================================
    let reported_union_fields_impl = quote! {
        impl #krate::shadows::ReportedUnionFields for #reported_name {
            const FIELD_NAMES: &'static [&'static str] = &[#tag_key];

            fn serialize_into_map<S: ::serde::ser::SerializeMap>(
                &self,
                _map: &mut S,
            ) -> Result<(), S::Error> {
                // Adjacently-tagged enums serialize via their custom Serialize impl
                Ok(())
            }
        }
    };

    Ok((
        delta_type,
        reported_type,
        shadow_node_impl,
        reported_union_fields_impl,
    ))
}
