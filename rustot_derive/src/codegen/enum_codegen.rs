//! Code generation for enum types (simple and adjacently-tagged)
//!
//! This module generates `ShadowNode` and `KVPersist` implementations for enum types.
//! Enums are routed to different codegen paths based on their serde tagging strategy.
//!
//! # Enum Tagging Strategies
//!
//! ## Externally Tagged (default)
//!
//! ```ignore
//! enum State { Off, On(Config) }
//! // JSON: {"Off": null} or {"On": {...config...}}
//! ```
//!
//! This is the serde default. The variant name is the JSON key.
//!
//! ## Internally Tagged
//!
//! ```ignore
//! #[serde(tag = "mode")]
//! enum State { Off, On }
//! // JSON: {"mode": "Off"} or {"mode": "On"}
//! ```
//!
//! Only works for unit variants or variants with struct data (not newtype/tuple).
//! The tag field appears inside the object alongside any variant data.
//!
//! **Detection**: `tag` attribute present, `content` attribute absent.
//!
//! ## Adjacently Tagged
//!
//! ```ignore
//! #[serde(tag = "mode", content = "config")]
//! enum State { Off, Running(Config) }
//! // JSON: {"mode": "Off"} or {"mode": "Running", "config": {...}}
//! ```
//!
//! The tag and content are sibling fields. This allows newtype variants.
//!
//! **Detection**: Both `tag` and `content` attributes present.
//! **Codegen**: Routed to [`adjacently_tagged`](super::adjacently_tagged) module.
//!
//! # Variant Types
//!
//! - **Unit variants**: `Off` - no associated data
//! - **Newtype variants**: `Running(Config)` - wraps a single type that implements `ShadowNode`
//! - **Struct variants**: Not currently supported (would need field-level handling)
//!
//! For newtype variants, the inner type must implement `ShadowNode`. The Delta/Reported
//! types delegate to the inner type's Delta/Reported.
//!
//! # KV Storage
//!
//! Enums use a special `/_variant` key to store the current variant name. Newtype variants
//! also store their inner data under `/{variant_name}/...` paths, delegating to the inner
//! type's `KVPersist` implementation.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Ident};

use crate::attr::{
    apply_rename_all, get_serde_rename, get_serde_rename_all, get_serde_tag_content,
    has_default_attr,
};

use super::adjacently_tagged::generate_adjacently_tagged_enum_code;
use super::helpers::{build_const_max_expr, build_max_key_len_expr};
use super::kv_codegen::{self, VARIANT_KEY_PATH};
use super::CodegenOutput;

/// Generate code for an enum type
pub(crate) fn generate_enum_code(
    input: &DeriveInput,
    delta_name: &Ident,
    reported_name: &Ident,
    is_adjacently_tagged: bool,
    krate: &TokenStream,
) -> syn::Result<CodegenOutput> {
    if is_adjacently_tagged {
        generate_adjacently_tagged_enum_code(input, delta_name, reported_name, krate)
    } else {
        generate_simple_enum_code(input, delta_name, reported_name, krate)
    }
}

/// Generate code for a simple (non-adjacently-tagged) enum type
pub(crate) fn generate_simple_enum_code(
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

    // Get rename_all from enum attributes
    let rename_all = get_serde_rename_all(&input.attrs);

    // Detect internally-tagged enums: #[serde(tag = "...")] without content.
    // Internally-tagged enums embed the tag inside the object, which only works
    // for unit variants or variants with struct data (not newtype).
    // For parse_delta, this affects how we locate the variant name in JSON.
    let (tag_key, content_key) = get_serde_tag_content(&input.attrs);
    let is_internally_tagged = tag_key.is_some() && content_key.is_none();
    let tag_key_str = tag_key.clone().unwrap_or_default();

    // Find default variant
    let default_variant = variants.iter().find(|v| has_default_attr(&v.attrs));

    // Track if we have any newtype variants (requires special parse_delta)
    let mut has_newtype_variants = false;
    let mut parse_delta_arms: Vec<TokenStream> = Vec::new();

    // Collect variant info
    let mut delta_variants = Vec::new();
    let mut reported_variants = Vec::new();
    let mut variant_names = Vec::new();
    let mut apply_delta_arms = Vec::new();
    let mut into_partial_reported_arms = Vec::new();
    let mut schema_hash_code = Vec::new();

    // KVPersist-specific codegen (feature-gated)
    let mut max_value_len_items = Vec::new();
    let mut load_from_kv_variant_arms = Vec::new();
    let mut persist_to_kv_variant_arms = Vec::new();
    let mut persist_delta_arms = Vec::new();
    let mut collect_valid_keys_arms = Vec::new();
    let mut collect_valid_prefixes_arms = Vec::new();
    let mut max_key_len_items = Vec::new();
    let mut variant_name_arms = Vec::new();

    let variant_key_len = VARIANT_KEY_PATH.len();

    for variant in variants {
        let variant_ident = &variant.ident;

        // Get serde rename for variant
        let serde_name = get_serde_rename(&variant.attrs).unwrap_or_else(|| {
            if let Some(ref convention) = rename_all {
                apply_rename_all(&variant_ident.to_string(), convention)
            } else {
                variant_ident.to_string()
            }
        });

        variant_names.push(serde_name.clone());

        match &variant.fields {
            Fields::Unit => {
                // Unit variant
                delta_variants.push(quote! { #variant_ident, });
                reported_variants.push(quote! { #variant_ident, });

                // apply_delta: just set the variant
                apply_delta_arms.push(quote! {
                    Self::Delta::#variant_ident => {
                        *self = Self::#variant_ident;
                    }
                });

                // into_partial_reported: for unit variants, return the reported variant
                into_partial_reported_arms.push(quote! {
                    Self::Delta::#variant_ident => Self::Reported::#variant_ident,
                });

                // Schema hash for unit variant
                let variant_bytes = serde_name.as_bytes();
                schema_hash_code.push(quote! {
                    h = #krate::shadows::fnv1a_bytes(h, &[#(#variant_bytes),*]);
                });

                // =====================================================================
                // KVPersist codegen for unit variant
                // =====================================================================

                variant_name_arms.push(quote! {
                    Self::#variant_ident => #serde_name,
                });

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

                // persist_delta: write _variant key
                persist_delta_arms.push(quote! {
                    Self::Delta::#variant_ident => {
                        let mut variant_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                        let _ = variant_key.push_str(prefix);
                        let _ = variant_key.push_str(#VARIANT_KEY_PATH);
                        kv.store(&variant_key, #serde_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;
                    }
                });

                // parse_delta arm for unit variant
                // Different handling for internally-tagged vs externally-tagged enums
                if is_internally_tagged {
                    // Internally-tagged: match on tag value
                    parse_delta_arms.push(quote! {
                        #serde_name => Ok(Self::Delta::#variant_ident),
                    });
                } else {
                    // Externally-tagged: check if variant name is a key
                    parse_delta_arms.push(quote! {
                        if scanner.field_bytes(#serde_name).is_some() {
                            return Ok(Self::Delta::#variant_ident);
                        }
                    });
                }
            }
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                // Newtype variant - delegate to inner ShadowNode
                let inner_ty = &fields.unnamed[0].ty;
                let delta_inner_ty = quote! { <#inner_ty as #krate::shadows::ShadowNode>::Delta };
                let reported_inner_ty =
                    quote! { <#inner_ty as #krate::shadows::ShadowNode>::Reported };

                delta_variants.push(quote! { #variant_ident(#delta_inner_ty), });
                reported_variants.push(quote! { #variant_ident(#reported_inner_ty), });

                // apply_delta: set variant and delegate to inner
                apply_delta_arms.push(quote! {
                    Self::Delta::#variant_ident(ref inner_delta) => {
                        if !matches!(self, Self::#variant_ident(_)) {
                            *self = Self::#variant_ident(Default::default());
                        }
                        if let Self::#variant_ident(ref mut inner) = self {
                            inner.apply_delta(inner_delta);
                        }
                    }
                });

                // into_partial_reported: delegate to inner's into_partial_reported
                // Note: apply_delta is called first, so self is already in the correct variant
                into_partial_reported_arms.push(quote! {
                    Self::Delta::#variant_ident(ref inner_delta) => {
                        match self {
                            Self::#variant_ident(ref inner) => {
                                Self::Reported::#variant_ident(inner.into_partial_reported(inner_delta))
                            }
                            _ => {
                                // Self not in expected variant - shouldn't happen if apply_delta was called first
                                // Fall back to default
                                Self::Reported::#variant_ident(Default::default())
                            }
                        }
                    }
                });

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

                // MAX_VALUE_LEN
                max_value_len_items.push(quote! {
                    <#inner_ty as #krate::shadows::KVPersist>::MAX_VALUE_LEN
                });

                variant_name_arms.push(quote! {
                    Self::#variant_ident(_) => #serde_name,
                });

                // load_from_kv arm: construct variant, delegate to inner
                load_from_kv_variant_arms.push(kv_codegen::enum_variant_load_arm(
                    krate,
                    &variant_path,
                    &serde_name,
                    variant_ident,
                    inner_ty,
                ));

                // persist_to_kv arm: delegate to inner
                persist_to_kv_variant_arms.push(kv_codegen::enum_variant_persist_arm(
                    krate,
                    &variant_path,
                    variant_ident,
                    inner_ty,
                ));

                // persist_delta: write _variant key and delegate to inner
                let variant_key_ident =
                    syn::Ident::new("variant_key", proc_macro2::Span::call_site());
                let variant_key_code = kv_codegen::build_key(&variant_key_ident, VARIANT_KEY_PATH);
                let inner_prefix_ident =
                    syn::Ident::new("inner_prefix", proc_macro2::Span::call_site());
                let inner_prefix_code = kv_codegen::build_key(&inner_prefix_ident, &variant_path);
                persist_delta_arms.push(quote! {
                    Self::Delta::#variant_ident(ref inner_delta) => {
                        #variant_key_code
                        kv.store(&#variant_key_ident, #serde_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;

                        #inner_prefix_code
                        <#inner_ty as #krate::shadows::KVPersist>::persist_delta::<K, KEY_LEN>(inner_delta, kv, &#inner_prefix_ident).await?;
                    }
                });

                // collect_valid_keys: delegate to inner (all variants, not just active)
                collect_valid_keys_arms.push(kv_codegen::nested_collect_keys(
                    krate,
                    &variant_path,
                    inner_ty,
                ));

                // collect_valid_prefixes: delegate to inner
                collect_valid_prefixes_arms.push(kv_codegen::nested_collect_prefixes(
                    krate,
                    &variant_path,
                    inner_ty,
                ));

                // Mark that we have newtype variants
                has_newtype_variants = true;

                // parse_delta arm for newtype variant - delegate to inner type's parse_delta
                // Different handling for internally-tagged vs externally-tagged enums
                if is_internally_tagged {
                    // Internally-tagged: tag field contains variant name, rest of JSON is variant data
                    parse_delta_arms.push(quote! {
                        #serde_name => {
                            // Build nested path for the inner type
                            let mut nested_path: ::heapless::String<128> = ::heapless::String::new();
                            let _ = nested_path.push_str(path);
                            if !path.is_empty() {
                                let _ = nested_path.push('/');
                            }
                            let _ = nested_path.push_str(#serde_name);
                            // Parse the whole JSON into the inner delta type
                            // The inner type's parse_delta will ignore the tag field
                            let inner_delta = <#inner_ty as #krate::shadows::ShadowNode>::parse_delta(
                                json,
                                &nested_path,
                                resolver
                            ).await?;
                            Ok(Self::Delta::#variant_ident(inner_delta))
                        }
                    });
                } else {
                    // Externally-tagged: variant name is the object key, value is variant data
                    parse_delta_arms.push(quote! {
                        if let Some(content_bytes) = scanner.field_bytes(#serde_name) {
                            // Build nested path for the inner type
                            let mut nested_path: ::heapless::String<128> = ::heapless::String::new();
                            let _ = nested_path.push_str(path);
                            if !path.is_empty() {
                                let _ = nested_path.push('/');
                            }
                            let _ = nested_path.push_str(#serde_name);
                            let inner_delta = <#inner_ty as #krate::shadows::ShadowNode>::parse_delta(
                                content_bytes,
                                &nested_path,
                                resolver
                            ).await?;
                            return Ok(Self::Delta::#variant_ident(inner_delta));
                        }
                    });
                }
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
    let max_value_len_expr = build_const_max_expr(max_value_len_items, quote! { 0 });

    // Build MAX_KEY_LEN const expression (max of _variant key and variant paths)
    let max_key_len_expr = build_max_key_len_expr(max_key_len_items, quote! { #variant_key_len });

    // Generate enum KVPersist method bodies using shared helpers
    let load_from_kv_body = kv_codegen::enum_load_from_kv_body(krate, &load_from_kv_variant_arms);
    let persist_to_kv_body =
        kv_codegen::enum_persist_to_kv_body(krate, &variant_name_arms, &persist_to_kv_variant_arms);
    let collect_valid_keys_body =
        kv_codegen::enum_collect_valid_keys_body(&collect_valid_keys_arms);

    // Build SCHEMA_HASH const
    let schema_hash_const = quote! {
        {
            let mut h = #krate::shadows::FNV1A_INIT;
            #(#schema_hash_code)*
            h
        }
    };

    // Copy serde attributes from input
    let serde_attrs: Vec<_> = input
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("serde"))
        .collect();

    // Generate default impl for delta if we have a default variant
    let delta_default_impl = if let Some(default_var) = default_variant {
        let default_var_ident = &default_var.ident;
        match &default_var.fields {
            Fields::Unit => quote! {
                impl Default for #delta_name {
                    fn default() -> Self {
                        Self::#default_var_ident
                    }
                }
            },
            Fields::Unnamed(_) => quote! {
                impl Default for #delta_name {
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

    // Generate default impl for reported if we have a default variant
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

    // Generate Delta type - only include Deserialize when we can use serde directly
    // (i.e., when there are no newtype variants). Struct deltas no longer have Deserialize,
    // so any enum with newtype variants needs custom parsing.
    let needs_custom_parsing = has_newtype_variants;
    let delta_type = if needs_custom_parsing {
        // No Deserialize - we use custom parse_delta
        quote! {
            #[derive(
                ::serde::Serialize,
                Clone,
            )]
            #(#serde_attrs)*
            #vis enum #delta_name {
                #(#delta_variants)*
            }

            #delta_default_impl
        }
    } else {
        // Can use serde directly
        quote! {
            #[derive(
                ::serde::Serialize,
                ::serde::Deserialize,
                Clone,
            )]
            #(#serde_attrs)*
            #vis enum #delta_name {
                #(#delta_variants)*
            }

            #delta_default_impl
        }
    };

    // Generate Reported type
    let reported_type = quote! {
        #[derive(::serde::Serialize, Clone)]
        #(#serde_attrs)*
        #vis enum #reported_name {
            #(#reported_variants)*
        }

        #reported_default_impl
    };

    // Generate variant_at_path match arms
    let variant_at_path_arms: Vec<TokenStream> = variant_names
        .iter()
        .enumerate()
        .map(|(i, serde_name)| {
            let variant_ident = &variants.iter().nth(i).unwrap().ident;
            let has_data = !matches!(&variants.iter().nth(i).unwrap().fields, Fields::Unit);
            if has_data {
                quote! {
                    Self::#variant_ident(_) => {
                        let mut s = ::heapless::String::<32>::new();
                        let _ = s.push_str(#serde_name);
                        Some(s)
                    }
                }
            } else {
                quote! {
                    Self::#variant_ident => {
                        let mut s = ::heapless::String::<32>::new();
                        let _ = s.push_str(#serde_name);
                        Some(s)
                    }
                }
            }
        })
        .collect();

    // Generate parse_delta body based on whether we need custom parsing
    let variant_name_strs: Vec<_> = variant_names.iter().map(|s| s.as_str()).collect();
    let parse_delta_body = if needs_custom_parsing {
        if is_internally_tagged {
            // Internally-tagged enum: tag field contains variant name
            // JSON: { "kind": "Wpa", "ssid": "...", "password": "..." }
            quote! {
                // Extract the tag field using FieldScanner
                let scanner = #krate::shadows::tag_scanner::FieldScanner::scan(json, &[#tag_key_str])
                    .map_err(#krate::shadows::ParseError::Scan)?;

                let tag_str = scanner.field_str(#tag_key_str)
                    .ok_or(#krate::shadows::ParseError::MissingVariant)?;

                match tag_str {
                    #(#parse_delta_arms)*
                    _ => Err(#krate::shadows::ParseError::UnknownVariant),
                }
            }
        } else {
            // Externally-tagged enum: variant name is the object key
            // JSON: { "Wpa": { "ssid": "...", "password": "..." } }
            quote! {
                // Scan for variant names as keys
                let scanner = #krate::shadows::tag_scanner::FieldScanner::scan(json, &[#(#variant_name_strs),*])
                    .map_err(#krate::shadows::ParseError::Scan)?;

                // Try each variant (arms include the if-let checks)
                #(#parse_delta_arms)*

                Err(#krate::shadows::ParseError::MissingVariant)
            }
        }
    } else {
        // Simple enums (unit variants only) use serde directly
        quote! {
            ::serde_json_core::from_slice(json)
                .map(|(v, _)| v)
                .map_err(|_| #krate::shadows::ParseError::Deserialize)
        }
    };

    // Generate ShadowNode impl (always available)
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
                    #parse_delta_body
                }
            }

            fn apply_delta(&mut self, delta: &Self::Delta) {
                match delta {
                    #(#apply_delta_arms)*
                }
            }

            fn variant_at_path(&self, path: &str) -> Option<::heapless::String<32>> {
                if path.is_empty() {
                    match self {
                        #(#variant_at_path_arms)*
                    }
                } else {
                    None
                }
            }

            fn into_partial_reported(&self, delta: &Self::Delta) -> Self::Reported {
                match delta {
                    #(#into_partial_reported_arms)*
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
                #load_from_kv_body
            }

            fn load_from_kv_with_migration<K: #krate::shadows::KVStore, const KEY_LEN: usize>(
                &mut self,
                prefix: &str,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<#krate::shadows::LoadFieldResult, #krate::shadows::KvError<K::Error>>> {
                // Enums don't have migration support at this level
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
                    match delta {
                        #(#persist_delta_arms)*
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
    };

    // Generate ReportedUnionFields impl for enum
    // For enums, we need to serialize the discriminant
    let reported_union_fields_impl = quote! {
        impl #krate::shadows::ReportedUnionFields for #reported_name {
            const FIELD_NAMES: &'static [&'static str] = &[];

            fn serialize_into_map<S: ::serde::ser::SerializeMap>(
                &self,
                _map: &mut S,
            ) -> Result<(), S::Error> {
                // Enums serialize their variant directly, not as individual fields
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
