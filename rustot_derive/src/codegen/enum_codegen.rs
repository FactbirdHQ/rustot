//! Code generation for enum types (simple and adjacently-tagged)

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Ident};

use crate::attr::{
    apply_rename_all, get_serde_rename, get_serde_rename_all, has_default_attr,
};

use super::adjacently_tagged::generate_adjacently_tagged_enum_code;

/// Generate code for an enum type
pub(crate) fn generate_enum_code(
    input: &DeriveInput,
    delta_name: &Ident,
    reported_name: &Ident,
    is_adjacently_tagged: bool,
    krate: &TokenStream,
) -> syn::Result<(TokenStream, TokenStream, TokenStream, TokenStream)> {
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
) -> syn::Result<(TokenStream, TokenStream, TokenStream, TokenStream)> {
    let name = &input.ident;
    let vis = &input.vis;

    let variants = match &input.data {
        Data::Enum(data) => &data.variants,
        _ => return Err(syn::Error::new_spanned(input, "expected enum")),
    };

    // Get rename_all from enum attributes
    let rename_all = get_serde_rename_all(&input.attrs);

    // Find default variant
    let default_variant = variants.iter().find(|v| has_default_attr(&v.attrs));

    // Collect variant info
    let mut delta_variants = Vec::new();
    let mut reported_variants = Vec::new();
    let mut variant_names = Vec::new();
    let mut set_variant_arms = Vec::new();
    let mut get_variant_arms = Vec::new();
    let mut variant_name_arms = Vec::new(); // Simple arms that return &str (not Result)
    let mut apply_persist_arms = Vec::new();
    let mut into_reported_arms = Vec::new();
    let mut schema_hash_code = Vec::new();
    let mut max_value_len_items = Vec::new();

    // New codegen collections for load_from_kv, persist_to_kv, collect_valid_keys
    let mut load_from_kv_variant_arms = Vec::new();
    let mut persist_to_kv_variant_arms = Vec::new();
    let mut collect_valid_keys_arms = Vec::new();
    let mut max_depth_items = Vec::new();
    let mut max_key_len_items = Vec::new();

    // "_variant" key path length
    let variant_key_len = "/_variant".len();

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

        // Check if default variant (for future use)
        let _is_default = has_default_attr(&variant.attrs);

        match &variant.fields {
            Fields::Unit => {
                // Unit variant
                delta_variants.push(quote! { #variant_ident, });
                reported_variants.push(quote! { #variant_ident, });

                set_variant_arms.push(quote! {
                    #serde_name => {
                        *self = Self::#variant_ident;
                        Ok(())
                    }
                });

                get_variant_arms.push(quote! {
                    Self::#variant_ident => Ok(#serde_name),
                });

                variant_name_arms.push(quote! {
                    Self::#variant_ident => #serde_name,
                });

                apply_persist_arms.push(quote! {
                    Self::Delta::#variant_ident => {
                        if !matches!(self, Self::#variant_ident) {
                            *self = Self::#variant_ident;
                            // Write _variant key
                            let mut variant_key: ::heapless::String<256> = ::heapless::String::new();
                            let _ = variant_key.push_str(prefix);
                            let _ = variant_key.push_str("/_variant");
                            kv.store(&variant_key, #serde_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;
                        }
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
                // New codegen: load_from_kv, persist_to_kv, collect_valid_keys for unit variant
                // =====================================================================

                // MAX_DEPTH: unit variants contribute 0
                max_depth_items.push(quote! { 0 });

                // MAX_KEY_LEN: unit variants have no inner fields (just _variant key)
                // Don't add extra here since _variant is handled at enum level

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

                // collect_valid_keys: unit variants have no extra keys
            }
            Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
                // Newtype variant
                let inner_ty = &fields.unnamed[0].ty;
                // All types now implement ShadowNode (primitives have Delta = Self),
                // so we always use the ShadowNode delegation pattern
                let is_leaf = false;

                if is_leaf {
                    delta_variants.push(quote! { #variant_ident(#inner_ty), });
                    reported_variants.push(quote! { #variant_ident(#inner_ty), });

                    set_variant_arms.push(quote! {
                        #serde_name => {
                            *self = Self::#variant_ident(Default::default());
                            Ok(())
                        }
                    });

                    get_variant_arms.push(quote! {
                        Self::#variant_ident(_) => Ok(#serde_name),
                    });

                    variant_name_arms.push(quote! {
                        Self::#variant_ident(_) => #serde_name,
                    });

                    apply_persist_arms.push(quote! {
                        Self::Delta::#variant_ident(ref inner_val) => {
                            if !matches!(self, Self::#variant_ident(_)) {
                                *self = Self::#variant_ident(Default::default());
                                // Write _variant key
                                let mut variant_key: ::heapless::String<256> = ::heapless::String::new();
                                let _ = variant_key.push_str(prefix);
                                let _ = variant_key.push_str("/_variant");
                                kv.store(&variant_key, #serde_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;
                            }
                            if let Self::#variant_ident(ref mut inner) = self {
                                *inner = inner_val.clone();
                                // Serialize and persist
                                let mut full_key: ::heapless::String<256> = ::heapless::String::new();
                                let _ = full_key.push_str(prefix);
                                let _ = full_key.push_str("/");
                                let _ = full_key.push_str(#serde_name);
                                match ::postcard::to_slice(inner_val, buf) {
                                    Ok(bytes) => {
                                        kv.store(&full_key, bytes).await.map_err(#krate::shadows::KvError::Kv)?;
                                    }
                                    Err(_) => return Err(#krate::shadows::KvError::Serialization),
                                }
                            }
                        }
                    });

                    into_reported_arms.push(quote! {
                        Self::#variant_ident(inner) => Self::Reported::#variant_ident(inner),
                    });

                    max_value_len_items.push(quote! {
                        <#inner_ty as ::postcard::experimental::max_size::MaxSize>::POSTCARD_MAX_SIZE
                    });

                    // =====================================================================
                    // New codegen: load_from_kv, persist_to_kv, collect_valid_keys for leaf newtype
                    // =====================================================================
                    let variant_path = format!("/{}", serde_name);
                    let variant_path_len = variant_path.len();

                    // MAX_DEPTH: leaf variants contribute 0
                    max_depth_items.push(quote! { 0 });

                    // MAX_KEY_LEN: "/VariantName" for the leaf value
                    max_key_len_items.push(quote! { #variant_path_len });

                    // load_from_kv arm: construct variant, load inner value
                    load_from_kv_variant_arms.push(quote! {
                        #serde_name => {
                            *self = Self::#variant_ident(Default::default());
                            // Load the inner leaf value
                            let mut inner_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                            let _ = inner_key.push_str(prefix);
                            let _ = inner_key.push_str(#variant_path);
                            if let Some(data) = kv.fetch(&inner_key, buf).await.map_err(#krate::shadows::KvError::Kv)? {
                                if let Self::#variant_ident(ref mut inner) = self {
                                    *inner = ::postcard::from_bytes(data).map_err(|_| #krate::shadows::KvError::Serialization)?;
                                    result.loaded += 1;
                                }
                            } else {
                                result.defaulted += 1;
                            }
                        }
                    });

                    // persist_to_kv arm: persist the inner leaf value
                    persist_to_kv_variant_arms.push(quote! {
                        Self::#variant_ident(ref inner) => {
                            let mut inner_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                            let _ = inner_key.push_str(prefix);
                            let _ = inner_key.push_str(#variant_path);
                            match ::postcard::to_slice(inner, buf) {
                                Ok(bytes) => kv.store(&inner_key, bytes).await.map_err(#krate::shadows::KvError::Kv)?,
                                Err(_) => return Err(#krate::shadows::KvError::Serialization),
                            }
                        }
                    });

                    // collect_valid_keys: add the inner key path
                    collect_valid_keys_arms.push(quote! {
                        {
                            let mut key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                            let _ = key.push_str(prefix);
                            let _ = key.push_str(#variant_path);
                            keys(&key);
                        }
                    });
                } else {
                    // Nested ShadowNode
                    let delta_inner_ty =
                        quote! { <#inner_ty as #krate::shadows::ShadowNode>::Delta };
                    let reported_inner_ty =
                        quote! { <#inner_ty as #krate::shadows::ShadowNode>::Reported };

                    delta_variants.push(quote! { #variant_ident(#delta_inner_ty), });
                    reported_variants.push(quote! { #variant_ident(#reported_inner_ty), });

                    set_variant_arms.push(quote! {
                        #serde_name => {
                            *self = Self::#variant_ident(Default::default());
                            Ok(())
                        }
                    });

                    get_variant_arms.push(quote! {
                        Self::#variant_ident(_) => Ok(#serde_name),
                    });

                    variant_name_arms.push(quote! {
                        Self::#variant_ident(_) => #serde_name,
                    });

                    apply_persist_arms.push(quote! {
                        Self::Delta::#variant_ident(ref inner_delta) => {
                            if !matches!(self, Self::#variant_ident(_)) {
                                *self = Self::#variant_ident(Default::default());
                                // Write _variant key
                                let mut variant_key: ::heapless::String<256> = ::heapless::String::new();
                                let _ = variant_key.push_str(prefix);
                                let _ = variant_key.push_str("/_variant");
                                kv.store(&variant_key, #serde_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;
                            }
                            if let Self::#variant_ident(ref mut inner) = self {
                                let mut nested_prefix: ::heapless::String<256> = ::heapless::String::new();
                                let _ = nested_prefix.push_str(prefix);
                                let _ = nested_prefix.push_str("/");
                                let _ = nested_prefix.push_str(#serde_name);
                                inner.apply_and_persist(inner_delta, &nested_prefix, kv, buf).await?;
                            }
                        }
                    });

                    into_reported_arms.push(quote! {
                        Self::#variant_ident(inner) => Self::Reported::#variant_ident(inner.into_reported()),
                    });

                    max_value_len_items.push(quote! {
                        <#inner_ty as #krate::shadows::ShadowNode>::MAX_VALUE_LEN
                    });

                    // =====================================================================
                    // New codegen: load_from_kv, persist_to_kv, collect_valid_keys for nested ShadowNode
                    // =====================================================================
                    let variant_path = format!("/{}", serde_name);
                    let variant_path_len = variant_path.len();

                    // MAX_DEPTH: 1 + nested depth
                    max_depth_items.push(quote! { <#inner_ty as #krate::shadows::ShadowNode>::MAX_DEPTH });

                    // MAX_KEY_LEN: "/VariantName" + nested MAX_KEY_LEN
                    max_key_len_items.push(quote! { #variant_path_len + <#inner_ty as #krate::shadows::ShadowNode>::MAX_KEY_LEN });

                    // load_from_kv arm: construct variant, delegate to inner
                    load_from_kv_variant_arms.push(quote! {
                        #serde_name => {
                            *self = Self::#variant_ident(Default::default());
                            if let Self::#variant_ident(ref mut inner) = self {
                                let mut inner_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                                let _ = inner_prefix.push_str(prefix);
                                let _ = inner_prefix.push_str(#variant_path);
                                let inner_result = inner.load_from_kv::<K, KEY_LEN>(&inner_prefix, kv, buf).await?;
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
                            inner.persist_to_kv::<K, KEY_LEN>(&inner_prefix, kv, buf).await?;
                        }
                    });

                    // collect_valid_keys: delegate to inner (all variants, not just active)
                    collect_valid_keys_arms.push(quote! {
                        {
                            let mut inner_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                            let _ = inner_prefix.push_str(prefix);
                            let _ = inner_prefix.push_str(#variant_path);
                            <#inner_ty as #krate::shadows::ShadowNode>::collect_valid_keys::<KEY_LEN>(&inner_prefix, keys);
                        }
                    });
                }

                // Schema hash for newtype variant
                let variant_bytes = serde_name.as_bytes();
                schema_hash_code.push(quote! {
                    h = #krate::shadows::fnv1a_bytes(h, &[#(#variant_bytes),*]);
                    h = #krate::shadows::fnv1a_u64(h, <#inner_ty as #krate::shadows::ShadowNode>::SCHEMA_HASH);
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

    // Build MAX_DEPTH const expression (1 + max of variant depths)
    let max_depth_expr = if max_depth_items.is_empty() {
        quote! { 1 }
    } else {
        let mut expr = max_depth_items[0].clone();
        for item in &max_depth_items[1..] {
            expr = quote! { const_max(#expr, #item) };
        }
        quote! {
            {
                const fn const_max(a: usize, b: usize) -> usize {
                    if a > b { a } else { b }
                }
                1 + #expr
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

    // Generate Delta type
    let delta_type = quote! {
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

    // Generate ShadowNode impl
    let shadow_node_impl = quote! {
        impl #krate::shadows::ShadowNode for #name {
            type Delta = #delta_name;
            type Reported = #reported_name;

            const MAX_DEPTH: usize = #max_depth_expr;
            const MAX_KEY_LEN: usize = #max_key_len_expr;
            const MAX_VALUE_LEN: usize = #max_value_len_expr;
            const SCHEMA_HASH: u64 = #schema_hash_const;

            fn apply_and_persist<K: #krate::shadows::KVStore>(
                &mut self,
                delta: &Self::Delta,
                prefix: &str,
                kv: &K,
                buf: &mut [u8],
            ) -> impl ::core::future::Future<Output = Result<(), #krate::shadows::KvError<K::Error>>> {
                async move {
                    match delta {
                        #(#apply_persist_arms)*
                    }
                    Ok(())
                }
            }

            fn into_reported(self) -> Self::Reported {
                match self {
                    #(#into_reported_arms)*
                }
            }

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
                buf: &mut [u8],
            ) -> impl ::core::future::Future<Output = Result<#krate::shadows::LoadFieldResult, #krate::shadows::KvError<K::Error>>> {
                async move {
                    let mut result = #krate::shadows::LoadFieldResult::default();

                    // Read _variant key
                    let mut variant_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = variant_key.push_str(prefix);
                    let _ = variant_key.push_str("/_variant");

                    let variant_name = match kv.fetch(&variant_key, buf).await.map_err(#krate::shadows::KvError::Kv)? {
                        Some(data) => core::str::from_utf8(data).map_err(|_| #krate::shadows::KvError::InvalidVariant)?,
                        None => {
                            // No variant stored, use default
                            *self = Self::default();
                            return Ok(result);
                        }
                    };

                    // Construct variant and load inner fields
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
                buf: &mut [u8],
            ) -> impl ::core::future::Future<Output = Result<#krate::shadows::LoadFieldResult, #krate::shadows::KvError<K::Error>>> {
                // Enums don't have migration support at this level
                self.load_from_kv::<K, KEY_LEN>(prefix, kv, buf)
            }

            fn persist_to_kv<K: #krate::shadows::KVStore, const KEY_LEN: usize>(
                &self,
                prefix: &str,
                kv: &K,
                buf: &mut [u8],
            ) -> impl ::core::future::Future<Output = Result<(), #krate::shadows::KvError<K::Error>>> {
                async move {
                    // Write _variant key
                    let mut variant_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = variant_key.push_str(prefix);
                    let _ = variant_key.push_str("/_variant");

                    match self {
                        #(#persist_to_kv_variant_arms)*
                    }

                    // Write variant name
                    let variant_name: &str = match self {
                        #(#variant_name_arms)*
                    };
                    kv.store(&variant_key, variant_name.as_bytes()).await.map_err(#krate::shadows::KvError::Kv)?;

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

    Ok((
        delta_type,
        reported_type,
        shadow_node_impl,
        reported_union_fields_impl,
    ))
}
