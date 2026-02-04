//! Code generation for struct types

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Ident};

use crate::attr::{get_serde_rename, DefaultValue, FieldAttrs};

use super::helpers::build_const_max_expr;
use super::kv_codegen;

/// Generate code for a struct type
pub(crate) fn generate_struct_code(
    input: &DeriveInput,
    delta_name: &Ident,
    reported_name: &Ident,
    krate: &TokenStream,
) -> syn::Result<(TokenStream, TokenStream, TokenStream, TokenStream)> {
    let name = &input.ident;
    let vis = &input.vis;

    let fields = match &input.data {
        Data::Struct(data) => &data.fields,
        _ => return Err(syn::Error::new_spanned(input, "expected struct")),
    };

    let named_fields = match fields {
        Fields::Named(f) => &f.named,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "only named fields are supported",
            ))
        }
    };

    // Collect field info
    let mut delta_fields = Vec::new();
    let mut reported_fields = Vec::new();
    let mut field_names = Vec::new();
    let mut apply_delta_arms = Vec::new();
    let mut into_partial_reported_arms = Vec::new();
    let mut schema_hash_code = Vec::new();
    let mut reported_serialize_arms = Vec::new();
    let mut opaque_field_types: Vec<syn::Type> = Vec::new();

    // For parse_delta: collect field parsing info
    let mut parse_delta_field_names: Vec<String> = Vec::new();
    let mut parse_delta_arms: Vec<TokenStream> = Vec::new();

    // KVPersist-specific codegen (feature-gated)
    let mut migration_arms = Vec::new();
    let mut default_arms = Vec::new();
    let mut max_value_len_items = Vec::new();
    let mut load_from_kv_arms = Vec::new();
    let mut load_from_kv_migration_arms = Vec::new();
    let mut persist_to_kv_arms = Vec::new();
    let mut persist_delta_arms = Vec::new();
    let mut collect_valid_keys_arms = Vec::new();
    let mut collect_valid_prefixes_arms = Vec::new();
    let mut max_key_len_items = Vec::new();

    for field in named_fields {
        let field_name = field.ident.as_ref().unwrap();
        let field_ty = &field.ty;
        let attrs = FieldAttrs::from_attrs(&field.attrs);

        // Get serde rename if present
        let serde_name = get_serde_rename(&field.attrs).unwrap_or_else(|| field_name.to_string());
        let field_path = format!("/{}", serde_name);

        field_names.push(serde_name.clone());

        // Check if this is a leaf type for KV operations.
        let has_migration = !attrs.migrate_from().is_empty();
        let is_leaf = attrs.opaque || has_migration;

        // Collect opaque field types for where clause
        if attrs.opaque {
            opaque_field_types.push(field_ty.clone());
        }

        // Delta field (unless report_only)
        if !attrs.report_only {
            let field_attrs = &field.attrs;
            let filtered_attrs: Vec<_> = field_attrs
                .iter()
                .filter(|a| !a.path().is_ident("shadow_attr"))
                .collect();

            if is_leaf {
                delta_fields.push(quote! {
                    #(#filtered_attrs)*
                    #[serde(skip_serializing_if = "Option::is_none")]
                    pub #field_name: Option<#field_ty>,
                });
            } else {
                let delta_field_ty = quote! { <#field_ty as #krate::shadows::ShadowNode>::Delta };
                delta_fields.push(quote! {
                    #(#filtered_attrs)*
                    #[serde(skip_serializing_if = "Option::is_none")]
                    pub #field_name: Option<#delta_field_ty>,
                });
            }
        }

        // Reported field (all fields)
        {
            let field_attrs = &field.attrs;
            let filtered_attrs: Vec<_> = field_attrs
                .iter()
                .filter(|a| !a.path().is_ident("shadow_attr"))
                .collect();

            if is_leaf {
                reported_fields.push(quote! {
                    #(#filtered_attrs)*
                    #[serde(skip_serializing_if = "Option::is_none")]
                    pub #field_name: Option<#field_ty>,
                });
            } else {
                let reported_field_ty =
                    quote! { <#field_ty as #krate::shadows::ShadowNode>::Reported };
                reported_fields.push(quote! {
                    #(#filtered_attrs)*
                    #[serde(skip_serializing_if = "Option::is_none")]
                    pub #field_name: Option<#reported_field_ty>,
                });
            }
        }

        // Apply delta (non-report_only fields)
        if !attrs.report_only {
            if is_leaf {
                apply_delta_arms.push(quote! {
                    if let Some(ref val) = delta.#field_name {
                        self.#field_name = val.clone();
                    }
                });
            } else {
                // Nested ShadowNode - delegate
                apply_delta_arms.push(quote! {
                    if let Some(ref inner_delta) = delta.#field_name {
                        self.#field_name.apply_delta(inner_delta);
                    }
                });
            }
        }

        // Into partial reported - only include fields present in delta
        if attrs.report_only {
            // Report-only fields are never in delta, so always None in partial
            into_partial_reported_arms.push(quote! {
                #field_name: None,
            });
        } else if is_leaf {
            into_partial_reported_arms.push(quote! {
                #field_name: if delta.#field_name.is_some() {
                    Some(self.#field_name.clone())
                } else {
                    None
                },
            });
        } else {
            into_partial_reported_arms.push(quote! {
                #field_name: if let Some(ref inner_delta) = delta.#field_name {
                    Some(self.#field_name.into_partial_reported(inner_delta))
                } else {
                    None
                },
            });
        }

        // Schema hash
        let field_name_bytes = serde_name.as_bytes();
        if is_leaf {
            let ty_name = quote!(#field_ty).to_string();
            let ty_bytes = ty_name.as_bytes();
            schema_hash_code.push(quote! {
                h = #krate::shadows::fnv1a_bytes(h, &[#(#field_name_bytes),*]);
                h = #krate::shadows::fnv1a_bytes(h, &[#(#ty_bytes),*]);
            });

            // MAX_VALUE_LEN for leaf types (KVPersist)
            max_value_len_items.push(quote! {
                <#field_ty as #krate::shadows::KVPersist>::MAX_VALUE_LEN
            });
        } else {
            schema_hash_code.push(quote! {
                h = #krate::shadows::fnv1a_bytes(h, &[#(#field_name_bytes),*]);
                h = #krate::shadows::fnv1a_u64(h, <#field_ty as #krate::shadows::ShadowNode>::SCHEMA_HASH);
            });

            // MAX_VALUE_LEN for nested types (KVPersist)
            max_value_len_items.push(quote! {
                <#field_ty as #krate::shadows::KVPersist>::MAX_VALUE_LEN
            });
        }

        // ReportedUnionFields serialize_into_map
        reported_serialize_arms.push(quote! {
            if let Some(ref val) = self.#field_name {
                map.serialize_entry(#serde_name, val)?;
            }
        });

        // parse_delta field parsing (for non-report_only fields)
        if !attrs.report_only {
            parse_delta_field_names.push(serde_name.clone());

            if is_leaf {
                // Leaf field: use serde directly
                parse_delta_arms.push(quote! {
                    if let Some(field_bytes) = scanner.field_bytes(#serde_name) {
                        delta.#field_name = Some(
                            ::serde_json_core::from_slice(field_bytes)
                                .map(|(v, _)| v)
                                .map_err(|_| #krate::shadows::ParseError::Deserialize)?
                        );
                    }
                });
            } else {
                // Nested ShadowNode: delegate to parse_delta
                parse_delta_arms.push(quote! {
                    if let Some(field_bytes) = scanner.field_bytes(#serde_name) {
                        let mut nested_path: ::heapless::String<128> = ::heapless::String::new();
                        let _ = nested_path.push_str(path);
                        if !path.is_empty() {
                            let _ = nested_path.push('/');
                        }
                        let _ = nested_path.push_str(#serde_name);
                        delta.#field_name = Some(
                            <#field_ty as #krate::shadows::ShadowNode>::parse_delta(
                                field_bytes,
                                &nested_path,
                                resolver
                            ).await?
                        );
                    }
                });
            }
        }

        // =====================================================================
        // KVPersist-specific codegen
        // =====================================================================

        // Migration sources
        let migrate_from = attrs.migrate_from();
        if !migrate_from.is_empty() {
            let from_keys: Vec<_> = migrate_from.iter().collect();
            let convert_expr = if let Some(convert) = attrs.migrate_convert() {
                quote! { Some(#convert) }
            } else {
                quote! { None }
            };

            migration_arms.push(quote! {
                #field_path => {
                    const SOURCES: &[#krate::shadows::MigrationSource] = &[
                        #(#krate::shadows::MigrationSource {
                            key: #from_keys,
                            convert: #convert_expr,
                        }),*
                    ];
                    SOURCES
                }
            });
        }

        // Custom defaults
        if let Some(ref default_val) = attrs.default_value {
            let value_expr = match default_val {
                DefaultValue::Literal(lit) => quote! { #lit },
                DefaultValue::Function(path) => quote! { #path() },
            };
            default_arms.push(quote! {
                #field_path => {
                    self.#field_name = #value_expr;
                    true
                }
            });
        }

        let field_path_len = field_path.len(); // "/field_name".len()

        if is_leaf {
            // Leaf field: direct KV operations

            // MAX_KEY_LEN: just the field name length
            max_key_len_items.push(quote! { #field_path_len });

            // load_from_kv
            let on_missing = quote! {
                <Self as #krate::shadows::KVPersist>::apply_field_default(self, #field_path);
                result.defaulted += 1;
            };
            load_from_kv_arms.push(kv_codegen::leaf_load(
                krate,
                &field_path,
                quote! { self.#field_name },
                on_missing,
            ));

            // load_from_kv_with_migration
            let migrate_from_vec = attrs.migrate_from();
            let migrate_from_keys: Vec<_> = migrate_from_vec.iter().collect();
            let migrate_convert = attrs.migrate_convert();
            let migration_code = kv_codegen::migration_fallback(
                krate,
                &field_path,
                quote! { self.#field_name },
                &migrate_from_keys,
                migrate_convert.as_ref(),
            );
            load_from_kv_migration_arms.push(kv_codegen::leaf_load_with_migration(
                krate,
                &field_path,
                quote! { self.#field_name },
                migration_code,
            ));

            // persist_to_kv
            persist_to_kv_arms.push(kv_codegen::leaf_persist(
                krate,
                &field_path,
                quote! { self.#field_name },
            ));

            // persist_delta (for non-report_only fields)
            if !attrs.report_only {
                persist_delta_arms.push(kv_codegen::leaf_persist_delta(
                    krate,
                    &field_path,
                    field_name,
                ));
            }

            // collect_valid_keys
            collect_valid_keys_arms.push(kv_codegen::leaf_collect_keys(&field_path));
        } else {
            // Nested ShadowNode field: delegate

            // MAX_KEY_LEN: "/field_name".len() + nested MAX_KEY_LEN
            max_key_len_items.push(
                quote! { #field_path_len + <#field_ty as #krate::shadows::KVPersist>::MAX_KEY_LEN },
            );

            // load_from_kv
            load_from_kv_arms.push(kv_codegen::nested_load(
                krate,
                &field_path,
                field_ty,
                quote! { &mut self.#field_name },
            ));

            // load_from_kv_with_migration
            load_from_kv_migration_arms.push(kv_codegen::nested_load_with_migration(
                krate,
                &field_path,
                field_ty,
                quote! { &mut self.#field_name },
            ));

            // persist_to_kv
            persist_to_kv_arms.push(kv_codegen::nested_persist(
                krate,
                &field_path,
                field_ty,
                quote! { &self.#field_name },
            ));

            // persist_delta (for non-report_only fields)
            if !attrs.report_only {
                persist_delta_arms.push(kv_codegen::nested_persist_delta(
                    krate,
                    &field_path,
                    field_ty,
                    field_name,
                ));
            }

            // collect_valid_keys
            collect_valid_keys_arms.push(kv_codegen::nested_collect_keys(
                krate,
                &field_path,
                field_ty,
            ));

            // collect_valid_prefixes
            collect_valid_prefixes_arms.push(kv_codegen::nested_collect_prefixes(
                krate,
                &field_path,
                field_ty,
            ));
        }
    }

    // Build max_value_len const expression
    let max_value_len_expr = build_const_max_expr(max_value_len_items, quote! { 0 });

    // Build MAX_KEY_LEN const expression (max of field paths)
    let max_key_len_expr = build_const_max_expr(max_key_len_items, quote! { 0 });

    // Build SCHEMA_HASH const
    let schema_hash_const = quote! {
        {
            let mut h = #krate::shadows::FNV1A_INIT;
            #(#schema_hash_code)*
            h
        }
    };

    // Collect all migration source keys for all_migration_keys()
    let mut all_migration_from_keys = Vec::new();
    for field in named_fields {
        let attrs = FieldAttrs::from_attrs(&field.attrs);
        for from_key in &attrs.migrate_from() {
            all_migration_from_keys.push(from_key.clone());
        }
    }

    // Generate Delta type - always use FieldScanner, no Deserialize needed
    let delta_type = quote! {
        #[derive(
            ::serde::Serialize,
            Clone,
            Default,
        )]
        #vis struct #delta_name {
            #(#delta_fields)*
        }
    };

    // Generate Reported type
    let reported_type = quote! {
        #[derive(::serde::Serialize, Clone, Default)]
        #vis struct #reported_name {
            #(#reported_fields)*
        }
    };

    // Migration arms default
    let migration_default = quote! {
        _ => &[]
    };

    // Default arms default
    let default_default = quote! {
        _ => false
    };

    // Generate where clause for opaque field types (KVPersist bound, feature-gated)
    let kv_where_clause = if opaque_field_types.is_empty() {
        quote! {}
    } else {
        let bounds = opaque_field_types.iter().map(|ty| {
            quote! { #ty: #krate::shadows::KVPersist }
        });
        quote! { where #(#bounds),* }
    };

    // Build variant_at_path delegation for nested ShadowNode fields
    let mut variant_at_path_arms = Vec::new();
    for field in named_fields {
        let field_name = field.ident.as_ref().unwrap();
        let field_ty = &field.ty;
        let attrs = FieldAttrs::from_attrs(&field.attrs);
        let has_migration = !attrs.migrate_from().is_empty();
        let is_leaf = attrs.opaque || has_migration;

        if !is_leaf {
            let serde_name =
                get_serde_rename(&field.attrs).unwrap_or_else(|| field_name.to_string());
            let field_prefix = format!("{}/", serde_name);
            variant_at_path_arms.push(quote! {
                if path.starts_with(#field_prefix) {
                    let rest = &path[#field_prefix.len()..];
                    if let Some(v) = <#field_ty as #krate::shadows::ShadowNode>::variant_at_path(&self.#field_name, rest) {
                        return Some(v);
                    }
                } else if path == #serde_name {
                    if let Some(v) = <#field_ty as #krate::shadows::ShadowNode>::variant_at_path(&self.#field_name, "") {
                        return Some(v);
                    }
                }
            });
        }
    }

    // Generate parse_delta body - always use FieldScanner
    let field_name_strs: Vec<_> = parse_delta_field_names.iter().map(|s| s.as_str()).collect();
    let parse_delta_body = quote! {
        let scanner = #krate::shadows::tag_scanner::FieldScanner::scan(json, &[#(#field_name_strs),*])
            .map_err(#krate::shadows::ParseError::Scan)?;

        let mut delta = Self::Delta::default();
        #(#parse_delta_arms)*
        Ok(delta)
    };

    // Generate ShadowNode impl (always available, no where clause needed)
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
                #(#apply_delta_arms)*
            }

            fn variant_at_path(&self, path: &str) -> Option<::heapless::String<32>> {
                #(#variant_at_path_arms)*
                None
            }

            fn into_partial_reported(&self, delta: &Self::Delta) -> Self::Reported {
                #reported_name {
                    #(#into_partial_reported_arms)*
                }
            }
        }

        // KVPersist impl (feature-gated)
        #[cfg(feature = "shadows_kv_persist")]
        impl #krate::shadows::KVPersist for #name #kv_where_clause {
            const MAX_KEY_LEN: usize = #max_key_len_expr;
            const MAX_VALUE_LEN: usize = #max_value_len_expr;

            fn migration_sources(field_path: &str) -> &'static [#krate::shadows::MigrationSource] {
                match field_path {
                    #(#migration_arms)*
                    #migration_default
                }
            }

            fn all_migration_keys() -> impl Iterator<Item = &'static str> {
                const MIGRATION_KEYS: &[&str] = &[#(#all_migration_from_keys),*];
                MIGRATION_KEYS.iter().copied()
            }

            fn apply_field_default(&mut self, field_path: &str) -> bool {
                match field_path {
                    #(#default_arms)*
                    #default_default
                }
            }

            fn load_from_kv<K: #krate::shadows::KVStore, const KEY_LEN: usize>(
                &mut self,
                prefix: &str,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<#krate::shadows::LoadFieldResult, #krate::shadows::KvError<K::Error>>> {
                async move {
                    let mut result = #krate::shadows::LoadFieldResult::default();
                    #(#load_from_kv_arms)*
                    Ok(result)
                }
            }

            fn load_from_kv_with_migration<K: #krate::shadows::KVStore, const KEY_LEN: usize>(
                &mut self,
                prefix: &str,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<#krate::shadows::LoadFieldResult, #krate::shadows::KvError<K::Error>>> {
                async move {
                    let mut result = #krate::shadows::LoadFieldResult::default();
                    #(#load_from_kv_migration_arms)*
                    Ok(result)
                }
            }

            fn persist_to_kv<K: #krate::shadows::KVStore, const KEY_LEN: usize>(
                &self,
                prefix: &str,
                kv: &K,
            ) -> impl ::core::future::Future<Output = Result<(), #krate::shadows::KvError<K::Error>>> {
                async move {
                    #(#persist_to_kv_arms)*
                    Ok(())
                }
            }

            fn persist_delta<K: #krate::shadows::KVStore, const KEY_LEN: usize>(
                delta: &Self::Delta,
                kv: &K,
                prefix: &str,
            ) -> impl ::core::future::Future<Output = Result<(), #krate::shadows::KvError<K::Error>>> {
                async move {
                    #(#persist_delta_arms)*
                    Ok(())
                }
            }

            fn collect_valid_keys<const KEY_LEN: usize>(prefix: &str, keys: &mut impl FnMut(&str)) {
                #(#collect_valid_keys_arms)*
            }

            fn collect_valid_prefixes<const KEY_LEN: usize>(prefix: &str, prefixes: &mut impl FnMut(&str)) {
                #(#collect_valid_prefixes_arms)*
            }
        }
    };

    // Generate ReportedUnionFields impl
    let reported_union_fields_impl = quote! {
        impl #krate::shadows::ReportedUnionFields for #reported_name {
            const FIELD_NAMES: &'static [&'static str] = &[#(#field_names),*];

            fn serialize_into_map<S: ::serde::ser::SerializeMap>(
                &self,
                map: &mut S,
            ) -> Result<(), S::Error> {
                #(#reported_serialize_arms)*
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
