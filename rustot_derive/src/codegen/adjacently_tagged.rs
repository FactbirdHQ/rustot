//! Code generation for adjacently-tagged enum types
//!
//! Adjacently-tagged enums use `#[serde(tag = "...", content = "...")]` to serialize
//! as a struct with separate fields for the discriminant and content. This is used
//! when variant data needs to be independently updatable from the variant selection.
//!
//! # Example
//!
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
//! # Why Adjacently-Tagged Needs Special Handling
//!
//! With adjacently-tagged enums, a delta can update:
//! 1. Just the variant (mode), without changing config
//! 2. Just the config, without changing variant
//! 3. Both mode and config together
//!
//! This requires a **struct delta** with optional `mode` and `config` fields, rather than
//! the enum delta used for externally-tagged enums.
//!
//! # Generated Types
//!
//! For `PortMode` above, this generates:
//!
//! - **`PortModeVariant`**: A unit enum for the discriminant (`Inactive`, `Sio`)
//!   - Used as `Delta.mode: Option<PortModeVariant>`
//!   - Serializes to the tag field value (e.g., `"inactive"`, `"sio"`)
//!
//! - **`DeltaPortModeConfig`**: Config delta enum (only variants with data)
//!   - `DeltaPortModeConfig::Sio(DeltaSioConfig)`
//!   - Used as `Delta.config: Option<DeltaPortModeConfig>`
//!   - Unit variants like `Inactive` have no config to delta
//!
//! - **`DeltaPortMode`**: Struct with optional fields
//!   - `mode: Option<PortModeVariant>` - changes which variant is active
//!   - `config: Option<DeltaPortModeConfig>` - updates inner data of active variant
//!
//! - **`ReportedPortMode`**: Matches the original enum structure
//!   - Custom `Serialize` impl to output adjacently-tagged format
//!
//! # KV Storage
//!
//! Uses the same `/_variant` key pattern as simple enums. When the variant has data,
//! inner fields are stored under `/{variant_name}/...` prefixes.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Ident};

use crate::attr::{
    get_serde_rename_all, get_serde_tag_content, get_variant_serde_name, has_default_attr,
};

#[cfg(feature = "kv_persist")]
use super::helpers::build_max_key_len_expr;
#[cfg(feature = "kv_persist")]
use super::kv_codegen::{self, VARIANT_KEY_PATH};
use super::CodegenOutput;

/// Generate code for an adjacently-tagged enum type
pub(crate) fn generate_adjacently_tagged_enum_code(
    input: &DeriveInput,
    delta_name: &Ident,
    reported_name: &Ident,
    krate: &TokenStream,
) -> syn::Result<CodegenOutput> {
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

    // For into_partial_reported - match on self and construct Reported directly
    let mut into_partial_reported_arms = Vec::new();

    // For schema hash
    let mut schema_hash_code = Vec::new();

    // For custom Serialize - flat union
    let mut inactive_variant_field_nulls = Vec::new(); // field names to null when not active

    // Track which variants have data (for DeltaConfig enum)
    let mut variants_with_data: Vec<(&syn::Variant, String)> = Vec::new();

    // For async parse_delta config arms (uses parse_delta instead of serde)
    let mut parse_delta_config_arms: Vec<TokenStream> = Vec::new();

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

                // Only switch to this variant if not already in it
                mode_switch_arms.push(quote! {
                    #variant_enum_name::#variant_ident => {
                        if !matches!(self, Self::#variant_ident) {
                            *self = Self::#variant_ident;
                        }
                    }
                });

                // into_partial_reported: unit variant has no inner state
                into_partial_reported_arms.push(quote! {
                    Self::#variant_ident => Self::Reported::#variant_ident,
                });

                // Schema hash for unit variant
                let variant_bytes = serde_name.as_bytes();
                schema_hash_code.push(quote! {
                    h = #krate::shadows::fnv1a_bytes(h, &[#(#variant_bytes),*]);
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

                // Only switch to this variant if not already in it
                mode_switch_arms.push(quote! {
                    #variant_enum_name::#variant_ident => {
                        if !matches!(self, Self::#variant_ident(_)) {
                            *self = Self::#variant_ident(Default::default());
                        }
                    }
                });

                config_apply_arms.push(quote! {
                    (Self::#variant_ident(ref mut inner), #delta_config_name::#variant_ident(ref delta_inner)) => {
                        inner.apply_delta(delta_inner);
                    }
                });

                // into_partial_reported: get inner delta from config if matching, else use default
                into_partial_reported_arms.push(quote! {
                    Self::#variant_ident(ref inner) => {
                        let inner_delta = match &delta.config {
                            Some(#delta_config_name::#variant_ident(d)) => d.clone(),
                            _ => Default::default(),
                        };
                        Self::Reported::#variant_ident(inner.into_partial_reported(&inner_delta))
                    }
                });

                // For flat union serialize - use ReportedUnionFields
                inactive_variant_field_nulls.push((variant_ident.clone(), inner_ty.clone()));

                // Schema hash for newtype variant
                let variant_bytes = serde_name.as_bytes();
                schema_hash_code.push(quote! {
                    h = #krate::shadows::fnv1a_bytes(h, &[#(#variant_bytes),*]);
                    h = #krate::shadows::fnv1a_u64(h, <#inner_ty as #krate::shadows::ShadowNode>::SCHEMA_HASH);
                });

                // parse_delta config arm - call parse_delta on inner type (async)
                parse_delta_config_arms.push(quote! {
                    (Some(#variant_enum_name::#variant_ident), Some(config_bytes)) => {
                        // Build nested path for the inner type
                        let mut nested_path: ::heapless::String<128> = ::heapless::String::new();
                        let _ = nested_path.push_str(path);
                        if !path.is_empty() {
                            let _ = nested_path.push('/');
                        }
                        let _ = nested_path.push_str(#serde_name);
                        let inner_delta = <#inner_ty as #krate::shadows::ShadowNode>::parse_delta(
                            config_bytes,
                            &nested_path,
                            resolver
                        ).await?;
                        Some(#delta_config_name::#variant_ident(inner_delta))
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

    // Generate FromStr match arms for variant enum
    let from_str_arms: Vec<TokenStream> = variant_idents
        .iter()
        .zip(variant_names.iter())
        .map(|(ident, serde_name)| {
            quote! {
                #serde_name => Ok(Self::#ident),
            }
        })
        .collect();

    let variant_enum_type = quote! {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize)]
        #rename_all_attr
        #vis enum #variant_enum_name {
            #(#variant_enum_variants)*
        }

        #variant_default_impl

        impl ::core::str::FromStr for #variant_enum_name {
            type Err = ();

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    #(#from_str_arms)*
                    _ => Err(()),
                }
            }
        }
    };

    // =========================================================================
    // 2. Generate Delta Config Enum (only variants with data)
    // =========================================================================
    let delta_config_type = if delta_config_variants.is_empty() {
        // No variants with data - don't generate the config enum
        TokenStream::new()
    } else {
        // No Deserialize - inner delta types don't have it, we use parse_delta instead
        quote! {
            #[derive(Clone, ::serde::Serialize)]
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

    // NOTE: from_json method removed - parsing is now done directly in parse_delta
    // which allows async calls to nested parse_delta methods

    // Generate Delta type (no Deserialize - uses parse_delta instead)
    let delta_type = quote! {
        #variant_enum_type

        #delta_config_type

        #[derive(Clone, Default, ::serde::Serialize)]
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

    // Build serialize match arms - content is nested under content_key with nulls for inactive variants
    let combined_serialize_arms: Vec<TokenStream> = variant_idents
        .iter()
        .enumerate()
        .map(|(i, variant_ident)| {
            let serde_name = &variant_names[i];

            // Generate null field serialization for other data variants (used inside nested content map)
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
                            &mut content_map
                        )?;
                    }
                })
                .collect();

            // Check if this variant has data
            let has_data = variants_with_data
                .iter()
                .any(|(v, _)| v.ident == *variant_ident);

            if has_data {
                // Variant with data - serialize mode and nested content with nulls
                quote! {
                    Self::#variant_ident(ref config) => {
                        map.serialize_entry(#tag_key, #serde_name)?;
                        // Create nested content map using a helper that implements Serialize
                        struct ContentWrapper<'a, C>(&'a C, core::marker::PhantomData<fn() -> ()>);
                        impl<'a, C: #krate::shadows::ReportedUnionFields + ::serde::Serialize> ::serde::Serialize for ContentWrapper<'a, C> {
                            fn serialize<SS>(&self, serializer: SS) -> Result<SS::Ok, SS::Error>
                            where
                                SS: ::serde::Serializer,
                            {
                                use ::serde::ser::SerializeMap;
                                let mut content_map = serializer.serialize_map(None)?;
                                self.0.serialize_into_map(&mut content_map)?;
                                #(#nulls)*
                                content_map.end()
                            }
                        }
                        map.serialize_entry(#content_key, &ContentWrapper(config, core::marker::PhantomData))?;
                    }
                }
            } else {
                // Unit variant - serialize mode and nested content map with only nulls
                if nulls.is_empty() {
                    // No data variants at all - just serialize mode
                    quote! {
                        Self::#variant_ident => {
                            map.serialize_entry(#tag_key, #serde_name)?;
                        }
                    }
                } else {
                    // Has other data variants - serialize mode and nested content with nulls only
                    quote! {
                        Self::#variant_ident => {
                            map.serialize_entry(#tag_key, #serde_name)?;
                            // Create nested content map with only null fields
                            struct NullContentWrapper;
                            impl ::serde::Serialize for NullContentWrapper {
                                fn serialize<SS>(&self, serializer: SS) -> Result<SS::Ok, SS::Error>
                                where
                                    SS: ::serde::Serializer,
                                {
                                    use ::serde::ser::SerializeMap;
                                    let mut content_map = serializer.serialize_map(None)?;
                                    #(#nulls)*
                                    content_map.end()
                                }
                            }
                            map.serialize_entry(#content_key, &NullContentWrapper)?;
                        }
                    }
                }
            }
        })
        .collect();

    // Total fields is now just 1 (mode) or 2 (mode + content) since content is nested
    let has_any_data_variants = !variants_with_data.is_empty();
    let total_fields_expr = if has_any_data_variants {
        quote! { 2usize } // mode + content
    } else {
        quote! { 1usize } // just mode
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

    // Generate variant_at_path match arms
    let variant_at_path_arms: Vec<TokenStream> = variant_idents
        .iter()
        .zip(variant_names.iter())
        .map(|(ident, serde_name)| {
            let has_data = variants_with_data.iter().any(|(v, _)| v.ident == *ident);
            if has_data {
                quote! {
                    Self::#ident(_) => {
                        let mut s = ::heapless::String::<32>::new();
                        let _ = s.push_str(#serde_name);
                        Some(s)
                    }
                }
            } else {
                quote! {
                    Self::#ident => {
                        let mut s = ::heapless::String::<32>::new();
                        let _ = s.push_str(#serde_name);
                        Some(s)
                    }
                }
            }
        })
        .collect();

    let shadow_node_impl = quote! {
        impl #krate::shadows::ShadowNode for #name {
            type Delta = #delta_name;
            type Reported = #reported_name;

            const SCHEMA_HASH: u64 = #schema_hash_const;

            fn parse_delta<R: #krate::shadows::VariantResolver>(
                json: &[u8],
                path: &str,
                resolver: &R,
            ) -> impl ::core::future::Future<Output = Result<Self::Delta, #krate::shadows::ParseError>> {
                async move {
                    // Scan JSON for tag and content fields
                    let scan = #krate::shadows::TaggedJsonScan::scan(json, #tag_key, #content_key)
                        .map_err(#krate::shadows::ParseError::Scan)?;

                    // Parse tag, using fallback from resolver if missing
                    let mode = match scan.tag_str() {
                        Some(s) => Some(s.parse::<#variant_enum_name>()
                            .map_err(|_| #krate::shadows::ParseError::UnknownVariant)?),
                        None => {
                            // Try to get fallback variant from resolver
                            resolver.resolve(path).await
                                .and_then(|s| s.parse::<#variant_enum_name>().ok())
                        }
                    };

                    // Parse config content using async parse_delta
                    let config = match (mode, scan.content_bytes()) {
                        #(#parse_delta_config_arms)*
                        // Unit variant with content (ignore), variant without content, or no mode
                        _ => None,
                    };

                    Ok(#delta_name { mode, config })
                }
            }

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

            fn variant_at_path(&self, path: &str) -> Option<::heapless::String<32>> {
                // Only match empty path (this level) or exact field path
                if path.is_empty() {
                    match self {
                        #(#variant_at_path_arms)*
                    }
                } else {
                    None
                }
            }

            fn into_partial_reported(&self, delta: &Self::Delta) -> Self::Reported {
                // For adjacently-tagged enums, always report full variant state
                // (the enum is the atomic unit - you can't partially report it)
                let _ = delta; // delta is used to get inner delta for newtype variants
                match self {
                    #(#into_partial_reported_arms)*
                }
            }
        }
    };

    // KVPersist impl (only generated when kv_persist feature is enabled)
    #[cfg(feature = "kv_persist")]
    let kv_persist_impl = {
        // Collect KV-specific codegen by iterating over variants again
        let mut load_from_kv_variant_arms = Vec::new();
        let mut persist_to_kv_variant_arms = Vec::new();
        let mut persist_delta_mode_arms = Vec::new();
        let mut persist_delta_config_arms = Vec::new();
        let mut collect_valid_keys_arms = Vec::new();
        let mut collect_valid_prefixes_arms = Vec::new();
        let mut max_key_len_items = Vec::new();
        let mut variant_name_arms_kv = Vec::new();

        let variant_key_len = VARIANT_KEY_PATH.len();

        for (variant, serde_name) in variants.iter().zip(variant_names.iter()) {
            let variant_ident = &variant.ident;

            match &variant.fields {
                Fields::Unit => {
                    variant_name_arms_kv.push(quote! {
                        Self::#variant_ident => #serde_name,
                    });

                    load_from_kv_variant_arms.push(quote! {
                        #serde_name => {
                            *self = Self::#variant_ident;
                        }
                    });

                    persist_to_kv_variant_arms.push(quote! {
                        Self::#variant_ident => {
                            // No inner fields to persist
                        }
                    });

                    persist_delta_mode_arms.push(quote! {
                        #variant_enum_name::#variant_ident => {
                            kv.store(&variant_key, #serde_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;
                        }
                    });
                }
                Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                    let inner_ty = &fields.unnamed[0].ty;
                    let variant_path = format!("/{}", serde_name);
                    let variant_path_len = variant_path.len();

                    max_key_len_items.push(quote! { #variant_path_len + <#inner_ty as #krate::shadows::KVPersist>::MAX_KEY_LEN });

                    variant_name_arms_kv.push(quote! {
                        Self::#variant_ident(_) => #serde_name,
                    });

                    load_from_kv_variant_arms.push(kv_codegen::enum_variant_load_arm(
                        krate,
                        &variant_path,
                        serde_name,
                        variant_ident,
                        inner_ty,
                    ));

                    persist_to_kv_variant_arms.push(kv_codegen::enum_variant_persist_arm(
                        krate,
                        &variant_path,
                        variant_ident,
                        inner_ty,
                    ));

                    persist_delta_mode_arms.push(quote! {
                        #variant_enum_name::#variant_ident => {
                            kv.store(&variant_key, #serde_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;
                        }
                    });

                    let prefix_ident =
                        syn::Ident::new("inner_prefix", proc_macro2::Span::call_site());
                    let prefix_code = kv_codegen::build_key(&prefix_ident, &variant_path);
                    persist_delta_config_arms.push(quote! {
                        #delta_config_name::#variant_ident(ref inner_delta) => {
                            #prefix_code
                            <#inner_ty as #krate::shadows::KVPersist>::persist_delta::<K, KEY_LEN>(inner_delta, kv, &#prefix_ident).await?;
                        }
                    });

                    collect_valid_keys_arms.push(kv_codegen::nested_collect_keys(
                        krate,
                        &variant_path,
                        inner_ty,
                    ));

                    collect_valid_prefixes_arms.push(kv_codegen::nested_collect_prefixes(
                        krate,
                        &variant_path,
                        inner_ty,
                    ));
                }
                _ => {}
            }
        }

        // Build const expressions
        // Adjacently-tagged enums use a fixed 128-byte buffer for _variant names, not ValueBuf.
        // Inner variant types bring their own ValueBuf.
        let max_key_len_expr =
            build_max_key_len_expr(max_key_len_items, quote! { #variant_key_len });

        // Generate method bodies using shared helpers
        let load_from_kv_body =
            kv_codegen::enum_load_from_kv_body(krate, &load_from_kv_variant_arms);
        let persist_to_kv_body = kv_codegen::enum_persist_to_kv_body(
            krate,
            &variant_name_arms_kv,
            &persist_to_kv_variant_arms,
        );
        let collect_valid_keys_body =
            kv_codegen::enum_collect_valid_keys_body(&collect_valid_keys_arms);

        quote! {
            impl #krate::shadows::KVPersist for #name {
                const MAX_KEY_LEN: usize = #max_key_len_expr;
                // Adjacently-tagged enums don't directly serialize values — variant name uses
                // fixed 128-byte buffer, inner types bring their own ValueBuf
                type ValueBuf = [u8; 0];
                fn zero_value_buf() -> Self::ValueBuf { [] }

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
                    #load_from_kv_body
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
                    #persist_to_kv_body
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
                        let _ = variant_key.push_str(#VARIANT_KEY_PATH);

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
                    #collect_valid_keys_body
                }

                fn collect_valid_prefixes<const KEY_LEN: usize>(prefix: &str, prefixes: &mut impl FnMut(&str)) {
                    #(#collect_valid_prefixes_arms)*
                }
            }
        }
    };

    #[cfg(not(feature = "kv_persist"))]
    let kv_persist_impl = quote! {};

    // Combine ShadowNode and KVPersist impls
    let shadow_node_impl = quote! {
        #shadow_node_impl
        #kv_persist_impl
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

    Ok(CodegenOutput {
        delta_type,
        reported_type,
        shadow_node_impl,
        reported_union_fields_impl,
    })
}
