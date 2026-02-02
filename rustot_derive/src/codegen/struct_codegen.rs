//! Code generation for struct types

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Ident};

use crate::attr::{get_serde_rename, DefaultValue, FieldAttrs};

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
    let mut into_reported_arms = Vec::new();
    let mut schema_hash_code = Vec::new();
    let mut reported_serialize_arms = Vec::new();
    let mut opaque_field_types: Vec<syn::Type> = Vec::new();

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
        let has_migration = !attrs.migrate_from.is_empty();
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

        // Into reported
        if is_leaf {
            into_reported_arms.push(quote! {
                #field_name: Some(self.#field_name),
            });
        } else {
            into_reported_arms.push(quote! {
                #field_name: Some(self.#field_name.into_reported()),
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

        // =====================================================================
        // KVPersist-specific codegen
        // =====================================================================

        // Migration sources
        if !attrs.migrate_from.is_empty() {
            let from_keys: Vec<_> = attrs.migrate_from.iter().collect();
            let convert_expr = if let Some(ref convert) = attrs.migrate_convert {
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
            load_from_kv_arms.push(quote! {
                {
                    let mut full_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = full_key.push_str(prefix);
                    let _ = full_key.push_str(#field_path);
                    #[cfg(not(feature = "std"))]
                    {
                        let mut __fetch_buf = [0u8; <Self as #krate::shadows::KVPersist>::MAX_VALUE_LEN];
                        if let Some(data) = kv.fetch(&full_key, &mut __fetch_buf).await.map_err(#krate::shadows::KvError::Kv)? {
                            self.#field_name = ::postcard::from_bytes(data).map_err(|_| #krate::shadows::KvError::Serialization)?;
                            result.loaded += 1;
                        } else {
                            <Self as #krate::shadows::KVPersist>::apply_field_default(self, #field_path);
                            result.defaulted += 1;
                        }
                    }
                    #[cfg(feature = "std")]
                    {
                        if let Some(data) = kv.fetch_to_vec(&full_key).await.map_err(#krate::shadows::KvError::Kv)? {
                            self.#field_name = ::postcard::from_bytes(&data).map_err(|_| #krate::shadows::KvError::Serialization)?;
                            result.loaded += 1;
                        } else {
                            <Self as #krate::shadows::KVPersist>::apply_field_default(self, #field_path);
                            result.defaulted += 1;
                        }
                    }
                }
            });

            // load_from_kv_with_migration
            let migrate_from_keys: Vec<_> = attrs.migrate_from.iter().collect();
            let migration_code = if migrate_from_keys.is_empty() {
                quote! {
                    // No migration sources, apply default
                    <Self as #krate::shadows::KVPersist>::apply_field_default(self, #field_path);
                    result.defaulted += 1;
                }
            } else {
                let convert_expr = if let Some(ref convert) = attrs.migrate_convert {
                    quote! { Some(#convert) }
                } else {
                    quote! { None }
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
                                self.#field_name = ::postcard::from_bytes(value_bytes).map_err(|_| #krate::shadows::KvError::Serialization)?;
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
                                self.#field_name = ::postcard::from_bytes(&value_bytes).map_err(|_| #krate::shadows::KvError::Serialization)?;
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
            };

            load_from_kv_migration_arms.push(quote! {
                {
                    let mut full_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = full_key.push_str(prefix);
                    let _ = full_key.push_str(#field_path);
                    #[cfg(not(feature = "std"))]
                    {
                        let mut __fetch_buf = [0u8; <Self as #krate::shadows::KVPersist>::MAX_VALUE_LEN];
                        if let Some(data) = kv.fetch(&full_key, &mut __fetch_buf).await.map_err(#krate::shadows::KvError::Kv)? {
                            match ::postcard::from_bytes(data) {
                                Ok(val) => {
                                    self.#field_name = val;
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
                        if let Some(data) = kv.fetch_to_vec(&full_key).await.map_err(#krate::shadows::KvError::Kv)? {
                            match ::postcard::from_bytes(&data) {
                                Ok(val) => {
                                    self.#field_name = val;
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
            });

            // persist_to_kv
            persist_to_kv_arms.push(quote! {
                {
                    let mut full_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = full_key.push_str(prefix);
                    let _ = full_key.push_str(#field_path);
                    #[cfg(not(feature = "std"))]
                    {
                        let mut __ser_buf = [0u8; <Self as #krate::shadows::KVPersist>::MAX_VALUE_LEN];
                        let bytes = ::postcard::to_slice(&self.#field_name, &mut __ser_buf)
                            .map_err(|_| #krate::shadows::KvError::Serialization)?;
                        kv.store(&full_key, bytes).await.map_err(#krate::shadows::KvError::Kv)?;
                    }
                    #[cfg(feature = "std")]
                    {
                        let bytes = ::postcard::to_allocvec(&self.#field_name)
                            .map_err(|_| #krate::shadows::KvError::Serialization)?;
                        kv.store(&full_key, &bytes).await.map_err(#krate::shadows::KvError::Kv)?;
                    }
                }
            });

            // persist_delta (for non-report_only fields)
            if !attrs.report_only {
                persist_delta_arms.push(quote! {
                    if let Some(ref val) = delta.#field_name {
                        let mut full_key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                        let _ = full_key.push_str(prefix);
                        let _ = full_key.push_str(#field_path);
                        #[cfg(not(feature = "std"))]
                        {
                            let mut __ser_buf = [0u8; <Self as #krate::shadows::KVPersist>::MAX_VALUE_LEN];
                            let bytes = ::postcard::to_slice(val, &mut __ser_buf)
                                .map_err(|_| #krate::shadows::KvError::Serialization)?;
                            kv.store(&full_key, bytes).await.map_err(#krate::shadows::KvError::Kv)?;
                        }
                        #[cfg(feature = "std")]
                        {
                            let bytes = ::postcard::to_allocvec(val)
                                .map_err(|_| #krate::shadows::KvError::Serialization)?;
                            kv.store(&full_key, &bytes).await.map_err(#krate::shadows::KvError::Kv)?;
                        }
                    }
                });
            }

            // collect_valid_keys
            collect_valid_keys_arms.push(quote! {
                {
                    let mut key: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = key.push_str(prefix);
                    let _ = key.push_str(#field_path);
                    keys(&key);
                }
            });
        } else {
            // Nested ShadowNode field: delegate

            // MAX_KEY_LEN: "/field_name".len() + nested MAX_KEY_LEN
            max_key_len_items.push(
                quote! { #field_path_len + <#field_ty as #krate::shadows::KVPersist>::MAX_KEY_LEN },
            );

            // load_from_kv
            load_from_kv_arms.push(quote! {
                {
                    let mut nested_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = nested_prefix.push_str(prefix);
                    let _ = nested_prefix.push_str(#field_path);
                    let inner = <#field_ty as #krate::shadows::KVPersist>::load_from_kv::<K, KEY_LEN>(&mut self.#field_name, &nested_prefix, kv).await?;
                    result.merge(inner);
                }
            });

            // load_from_kv_with_migration
            load_from_kv_migration_arms.push(quote! {
                {
                    let mut nested_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = nested_prefix.push_str(prefix);
                    let _ = nested_prefix.push_str(#field_path);
                    let inner = <#field_ty as #krate::shadows::KVPersist>::load_from_kv_with_migration::<K, KEY_LEN>(&mut self.#field_name, &nested_prefix, kv).await?;
                    result.merge(inner);
                }
            });

            // persist_to_kv
            persist_to_kv_arms.push(quote! {
                {
                    let mut nested_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = nested_prefix.push_str(prefix);
                    let _ = nested_prefix.push_str(#field_path);
                    <#field_ty as #krate::shadows::KVPersist>::persist_to_kv::<K, KEY_LEN>(&self.#field_name, &nested_prefix, kv).await?;
                }
            });

            // persist_delta (for non-report_only fields)
            if !attrs.report_only {
                persist_delta_arms.push(quote! {
                    if let Some(ref inner_delta) = delta.#field_name {
                        let mut nested_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                        let _ = nested_prefix.push_str(prefix);
                        let _ = nested_prefix.push_str(#field_path);
                        <#field_ty as #krate::shadows::KVPersist>::persist_delta::<K, KEY_LEN>(inner_delta, kv, &nested_prefix).await?;
                    }
                });
            }

            // collect_valid_keys
            collect_valid_keys_arms.push(quote! {
                {
                    let mut nested_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = nested_prefix.push_str(prefix);
                    let _ = nested_prefix.push_str(#field_path);
                    <#field_ty as #krate::shadows::KVPersist>::collect_valid_keys::<KEY_LEN>(&nested_prefix, keys);
                }
            });

            // collect_valid_prefixes
            collect_valid_prefixes_arms.push(quote! {
                {
                    let mut nested_prefix: ::heapless::String<KEY_LEN> = ::heapless::String::new();
                    let _ = nested_prefix.push_str(prefix);
                    let _ = nested_prefix.push_str(#field_path);
                    <#field_ty as #krate::shadows::KVPersist>::collect_valid_prefixes::<KEY_LEN>(&nested_prefix, prefixes);
                }
            });
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

    // Build MAX_KEY_LEN const expression (max of field paths)
    let max_key_len_expr = if max_key_len_items.is_empty() {
        quote! { 0 }
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
                #expr
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

    // Collect all migration source keys for all_migration_keys()
    let mut all_migration_from_keys = Vec::new();
    for field in named_fields {
        let attrs = FieldAttrs::from_attrs(&field.attrs);
        for from_key in &attrs.migrate_from {
            all_migration_from_keys.push(from_key.clone());
        }
    }

    // Generate Delta type
    let delta_type = quote! {
        #[derive(
            ::serde::Serialize,
            ::serde::Deserialize,
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

    // Generate ShadowNode impl (always available, no where clause needed)
    let shadow_node_impl = quote! {
        impl #krate::shadows::ShadowNode for #name {
            type Delta = #delta_name;
            type Reported = #reported_name;

            const SCHEMA_HASH: u64 = #schema_hash_const;

            fn apply_delta(&mut self, delta: &Self::Delta) {
                #(#apply_delta_arms)*
            }

            fn into_reported(self) -> Self::Reported {
                #reported_name {
                    #(#into_reported_arms)*
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
