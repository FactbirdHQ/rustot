//! Code generation for struct types
//!
//! This module generates the `ShadowNode` and `KVPersist` implementations for struct types.
//!
//! # Field Classification: Leaf vs Nested
//!
//! Each field in a struct is classified as either a **leaf** or **nested** field.
//! This classification fundamentally affects code generation:
//!
//! ## Leaf Fields
//!
//! A field is a leaf if **any** of these conditions hold:
//! - `#[shadow_attr(opaque)]` - explicitly marks the field as opaque
//! - `#[shadow_attr(migrate_from = "...")]` - has migration sources
//!
//! Leaf fields are treated as atomic values:
//! - **Delta type**: `Option<FieldType>` (the field's own type wrapped in Option)
//! - **Reported type**: `Option<FieldType>`
//! - **KV storage**: Single key `/field_name` storing the serialized value
//! - **Schema hash**: Hashes field name + type name
//!
//! **Why migrations imply leaf**: Migration logic operates on the raw serialized bytes,
//! applying optional conversion functions. This only makes sense for atomic values,
//! not for nested structures that delegate to their own KV paths.
//!
//! ## Nested Fields
//!
//! Fields without `opaque` or `migrate_from` are nested. They must implement `ShadowNode`:
//! - **Delta type**: `Option<<FieldType as ShadowNode>::Delta>`
//! - **Reported type**: `Option<<FieldType as ShadowNode>::Reported>`
//! - **KV storage**: Delegates to inner type with prefix `/field_name`
//! - **Schema hash**: Hashes field name + inner type's `SCHEMA_HASH`
//!
//! # Other Field Attributes
//!
//! - `#[shadow_attr(report_only)]`: Field is excluded from Delta type (not in desired state,
//!   only reported). It will always be `None` in partial reported.
//! - `#[shadow_attr(default = ...)]`: Custom default value when KV key is missing.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, Ident};

use crate::attr::{get_serde_rename, DefaultValue, FieldAttrs};

use super::helpers::build_const_max_expr;
use super::kv_codegen;
use super::CodegenOutput;

/// All generated code fragments for a single struct field.
///
/// This struct captures the complete output of processing one field,
/// making it easier to understand what code is generated for each field
/// without tracking ~20 separate collections.
struct FieldCodegen {
    /// The serde field name (for FIELD_NAMES constant)
    serde_name: String,

    /// Delta struct field definition (None if report_only)
    delta_field: Option<TokenStream>,
    /// Reported struct field definition
    reported_field: TokenStream,

    /// apply_delta() arm (None if report_only)
    apply_delta_arm: Option<TokenStream>,
    /// into_partial_reported() field assignment
    into_partial_reported_arm: TokenStream,
    /// SCHEMA_HASH computation code
    schema_hash_code: TokenStream,
    /// ReportedUnionFields::serialize_into_map arm
    reported_serialize_arm: TokenStream,
    /// variant_at_path() delegation arm (None for leaf fields)
    variant_at_path_arm: Option<TokenStream>,

    /// Field name for parse_delta (None if report_only)
    parse_delta_field_name: Option<String>,
    /// parse_delta() field parsing arm (None if report_only)
    parse_delta_arm: Option<TokenStream>,

    // KVPersist codegen
    /// migration_sources() match arm (None if no migrations)
    migration_arm: Option<TokenStream>,
    /// apply_field_default() match arm (None if no custom default)
    default_arm: Option<TokenStream>,
    /// MAX_VALUE_LEN expression component
    max_value_len_item: TokenStream,
    /// MAX_KEY_LEN expression component
    max_key_len_item: TokenStream,
    /// load_from_kv() arm
    load_from_kv_arm: TokenStream,
    /// load_from_kv_with_migration() arm
    load_from_kv_migration_arm: TokenStream,
    /// persist_to_kv() arm
    persist_to_kv_arm: TokenStream,
    /// persist_delta() arm (None if report_only)
    persist_delta_arm: Option<TokenStream>,
    /// collect_valid_keys() arm
    collect_valid_keys_arm: TokenStream,
    /// collect_valid_prefixes() arm (None for leaf fields)
    collect_valid_prefixes_arm: Option<TokenStream>,

    /// Migration source keys (for all_migration_keys)
    migration_from_keys: Vec<String>,
    /// Opaque field type for where clause (None if not opaque)
    opaque_field_type: Option<syn::Type>,
}

/// Process a single struct field and generate all code fragments.
///
/// # Leaf vs Nested fields
///
/// A field is considered a "leaf" if either:
/// - It has the `#[shadow_attr(opaque)]` attribute, OR
/// - It has migration sources via `#[shadow_attr(migrate_from = ...)]`
///
/// Leaf fields are serialized directly as their type. Nested fields delegate
/// to their inner ShadowNode implementation for Delta/Reported types and
/// KV operations.
fn process_field(field: &syn::Field, krate: &TokenStream) -> FieldCodegen {
    let field_name = field.ident.as_ref().unwrap();
    let field_ty = &field.ty;
    let attrs = FieldAttrs::from_attrs(&field.attrs);

    let serde_name = get_serde_rename(&field.attrs).unwrap_or_else(|| field_name.to_string());
    let field_path = format!("/{}", serde_name);

    // A field is a "leaf" (direct KV storage) if it's opaque OR has migrations.
    // Fields with migrations must be leaves because migration logic operates on
    // the serialized value directly.
    let has_migration = !attrs.migrate_from().is_empty();
    let is_leaf = attrs.opaque || has_migration;

    // Filter out shadow_attr from forwarded attributes
    let filtered_attrs: Vec<_> = field
        .attrs
        .iter()
        .filter(|a| !a.path().is_ident("shadow_attr"))
        .collect();

    // --- Delta field ---
    let delta_field = if attrs.report_only {
        None
    } else if is_leaf {
        Some(quote! {
            #(#filtered_attrs)*
            #[serde(skip_serializing_if = "Option::is_none")]
            pub #field_name: Option<#field_ty>,
        })
    } else {
        let delta_field_ty = quote! { <#field_ty as #krate::shadows::ShadowNode>::Delta };
        Some(quote! {
            #(#filtered_attrs)*
            #[serde(skip_serializing_if = "Option::is_none")]
            pub #field_name: Option<#delta_field_ty>,
        })
    };

    // --- Reported field ---
    let reported_field = if is_leaf {
        quote! {
            #(#filtered_attrs)*
            #[serde(skip_serializing_if = "Option::is_none")]
            pub #field_name: Option<#field_ty>,
        }
    } else {
        let reported_field_ty = quote! { <#field_ty as #krate::shadows::ShadowNode>::Reported };
        quote! {
            #(#filtered_attrs)*
            #[serde(skip_serializing_if = "Option::is_none")]
            pub #field_name: Option<#reported_field_ty>,
        }
    };

    // --- apply_delta arm ---
    let apply_delta_arm = if attrs.report_only {
        None
    } else if is_leaf {
        Some(quote! {
            if let Some(ref val) = delta.#field_name {
                self.#field_name = val.clone();
            }
        })
    } else {
        Some(quote! {
            if let Some(ref inner_delta) = delta.#field_name {
                self.#field_name.apply_delta(inner_delta);
            }
        })
    };

    // --- into_partial_reported arm ---
    let into_partial_reported_arm = if attrs.report_only {
        quote! { #field_name: None, }
    } else if is_leaf {
        quote! {
            #field_name: if delta.#field_name.is_some() {
                Some(self.#field_name.clone())
            } else {
                None
            },
        }
    } else {
        quote! {
            #field_name: if let Some(ref inner_delta) = delta.#field_name {
                Some(self.#field_name.into_partial_reported(inner_delta))
            } else {
                None
            },
        }
    };

    // --- Schema hash code ---
    let field_name_bytes = serde_name.as_bytes();
    let schema_hash_code = if is_leaf {
        let ty_name = quote!(#field_ty).to_string();
        let ty_bytes = ty_name.as_bytes();
        quote! {
            h = #krate::shadows::fnv1a_bytes(h, &[#(#field_name_bytes),*]);
            h = #krate::shadows::fnv1a_bytes(h, &[#(#ty_bytes),*]);
        }
    } else {
        quote! {
            h = #krate::shadows::fnv1a_bytes(h, &[#(#field_name_bytes),*]);
            h = #krate::shadows::fnv1a_u64(h, <#field_ty as #krate::shadows::ShadowNode>::SCHEMA_HASH);
        }
    };

    // --- MAX_VALUE_LEN item ---
    let max_value_len_item = quote! {
        <#field_ty as #krate::shadows::KVPersist>::MAX_VALUE_LEN
    };

    // --- reported serialize arm ---
    let reported_serialize_arm = quote! {
        if let Some(ref val) = self.#field_name {
            map.serialize_entry(#serde_name, val)?;
        }
    };

    // --- parse_delta ---
    let (parse_delta_field_name, parse_delta_arm) = if attrs.report_only {
        (None, None)
    } else if is_leaf {
        (
            Some(serde_name.clone()),
            Some(quote! {
                if let Some(field_bytes) = scanner.field_bytes(#serde_name) {
                    delta.#field_name = Some(
                        ::serde_json_core::from_slice(field_bytes)
                            .map(|(v, _)| v)
                            .map_err(|_| #krate::shadows::ParseError::Deserialize)?
                    );
                }
            }),
        )
    } else {
        (
            Some(serde_name.clone()),
            Some(quote! {
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
            }),
        )
    };

    // --- variant_at_path arm (only for nested fields) ---
    let variant_at_path_arm = if is_leaf {
        None
    } else {
        let field_prefix = format!("{}/", serde_name);
        Some(quote! {
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
        })
    };

    // =======================================================================
    // KVPersist codegen
    // =======================================================================

    // --- Migration arm ---
    let migrate_from = attrs.migrate_from();
    let migration_from_keys: Vec<String> = migrate_from.iter().cloned().collect();
    let migration_arm = if migrate_from.is_empty() {
        None
    } else {
        let from_keys: Vec<_> = migrate_from.iter().collect();
        let convert_expr = if let Some(convert) = attrs.migrate_convert() {
            quote! { Some(#convert) }
        } else {
            quote! { None }
        };
        Some(quote! {
            #field_path => {
                const SOURCES: &[#krate::shadows::MigrationSource] = &[
                    #(#krate::shadows::MigrationSource {
                        key: #from_keys,
                        convert: #convert_expr,
                    }),*
                ];
                SOURCES
            }
        })
    };

    // --- Default arm ---
    let default_arm = attrs.default_value.as_ref().map(|default_val| {
        let value_expr = match default_val {
            DefaultValue::Literal(lit) => quote! { #lit },
            DefaultValue::Function(path) => quote! { #path() },
        };
        quote! {
            #field_path => {
                self.#field_name = #value_expr;
                true
            }
        }
    });

    let field_path_len = field_path.len();

    // --- KV operations (leaf vs nested) ---
    let (
        max_key_len_item,
        load_from_kv_arm,
        load_from_kv_migration_arm,
        persist_to_kv_arm,
        persist_delta_arm,
        collect_valid_keys_arm,
        collect_valid_prefixes_arm,
    ) = if is_leaf {
        // Leaf field: direct KV operations
        let max_key_len_item = quote! { #field_path_len };

        let on_missing = quote! {
            <Self as #krate::shadows::KVPersist>::apply_field_default(self, #field_path);
            result.defaulted += 1;
        };
        let load_from_kv_arm =
            kv_codegen::leaf_load(krate, &field_path, quote! { self.#field_name }, on_missing);

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
        let load_from_kv_migration_arm = kv_codegen::leaf_load_with_migration(
            krate,
            &field_path,
            quote! { self.#field_name },
            migration_code,
        );

        let persist_to_kv_arm =
            kv_codegen::leaf_persist(krate, &field_path, quote! { self.#field_name });

        let persist_delta_arm = if attrs.report_only {
            None
        } else {
            Some(kv_codegen::leaf_persist_delta(
                krate,
                &field_path,
                field_name,
            ))
        };

        let collect_valid_keys_arm = kv_codegen::leaf_collect_keys(&field_path);

        (
            max_key_len_item,
            load_from_kv_arm,
            load_from_kv_migration_arm,
            persist_to_kv_arm,
            persist_delta_arm,
            collect_valid_keys_arm,
            None, // leaf fields don't have prefixes
        )
    } else {
        // Nested ShadowNode field: delegate
        let max_key_len_item =
            quote! { #field_path_len + <#field_ty as #krate::shadows::KVPersist>::MAX_KEY_LEN };

        let load_from_kv_arm = kv_codegen::nested_load(
            krate,
            &field_path,
            field_ty,
            quote! { &mut self.#field_name },
        );

        let load_from_kv_migration_arm = kv_codegen::nested_load_with_migration(
            krate,
            &field_path,
            field_ty,
            quote! { &mut self.#field_name },
        );

        let persist_to_kv_arm =
            kv_codegen::nested_persist(krate, &field_path, field_ty, quote! { &self.#field_name });

        let persist_delta_arm = if attrs.report_only {
            None
        } else {
            Some(kv_codegen::nested_persist_delta(
                krate,
                &field_path,
                field_ty,
                field_name,
            ))
        };

        let collect_valid_keys_arm = kv_codegen::nested_collect_keys(krate, &field_path, field_ty);

        let collect_valid_prefixes_arm = Some(kv_codegen::nested_collect_prefixes(
            krate,
            &field_path,
            field_ty,
        ));

        (
            max_key_len_item,
            load_from_kv_arm,
            load_from_kv_migration_arm,
            persist_to_kv_arm,
            persist_delta_arm,
            collect_valid_keys_arm,
            collect_valid_prefixes_arm,
        )
    };

    // --- Opaque field type ---
    let opaque_field_type = if attrs.opaque {
        Some(field_ty.clone())
    } else {
        None
    };

    FieldCodegen {
        serde_name,
        delta_field,
        reported_field,
        apply_delta_arm,
        into_partial_reported_arm,
        schema_hash_code,
        reported_serialize_arm,
        variant_at_path_arm,
        parse_delta_field_name,
        parse_delta_arm,
        migration_arm,
        default_arm,
        max_value_len_item,
        max_key_len_item,
        load_from_kv_arm,
        load_from_kv_migration_arm,
        persist_to_kv_arm,
        persist_delta_arm,
        collect_valid_keys_arm,
        collect_valid_prefixes_arm,
        migration_from_keys,
        opaque_field_type,
    }
}

/// Generate code for a struct type
pub(crate) fn generate_struct_code(
    input: &DeriveInput,
    delta_name: &Ident,
    reported_name: &Ident,
    krate: &TokenStream,
) -> syn::Result<CodegenOutput> {
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

    // Process all fields and collect their codegen output
    let field_codegens: Vec<FieldCodegen> = named_fields
        .iter()
        .map(|field| process_field(field, krate))
        .collect();

    // Extract vectors from FieldCodegen structs
    let field_names: Vec<_> = field_codegens
        .iter()
        .map(|f| f.serde_name.clone())
        .collect();
    let delta_fields: Vec<_> = field_codegens
        .iter()
        .filter_map(|f| f.delta_field.clone())
        .collect();
    let reported_fields: Vec<_> = field_codegens
        .iter()
        .map(|f| f.reported_field.clone())
        .collect();
    let apply_delta_arms: Vec<_> = field_codegens
        .iter()
        .filter_map(|f| f.apply_delta_arm.clone())
        .collect();
    let into_partial_reported_arms: Vec<_> = field_codegens
        .iter()
        .map(|f| f.into_partial_reported_arm.clone())
        .collect();
    let schema_hash_code: Vec<_> = field_codegens
        .iter()
        .map(|f| f.schema_hash_code.clone())
        .collect();
    let reported_serialize_arms: Vec<_> = field_codegens
        .iter()
        .map(|f| f.reported_serialize_arm.clone())
        .collect();
    let variant_at_path_arms: Vec<_> = field_codegens
        .iter()
        .filter_map(|f| f.variant_at_path_arm.clone())
        .collect();
    let parse_delta_field_names: Vec<_> = field_codegens
        .iter()
        .filter_map(|f| f.parse_delta_field_name.clone())
        .collect();
    let parse_delta_arms: Vec<_> = field_codegens
        .iter()
        .filter_map(|f| f.parse_delta_arm.clone())
        .collect();

    // KVPersist collections
    let migration_arms: Vec<_> = field_codegens
        .iter()
        .filter_map(|f| f.migration_arm.clone())
        .collect();
    let default_arms: Vec<_> = field_codegens
        .iter()
        .filter_map(|f| f.default_arm.clone())
        .collect();
    let max_value_len_items: Vec<_> = field_codegens
        .iter()
        .map(|f| f.max_value_len_item.clone())
        .collect();
    let max_key_len_items: Vec<_> = field_codegens
        .iter()
        .map(|f| f.max_key_len_item.clone())
        .collect();
    let load_from_kv_arms: Vec<_> = field_codegens
        .iter()
        .map(|f| f.load_from_kv_arm.clone())
        .collect();
    let load_from_kv_migration_arms: Vec<_> = field_codegens
        .iter()
        .map(|f| f.load_from_kv_migration_arm.clone())
        .collect();
    let persist_to_kv_arms: Vec<_> = field_codegens
        .iter()
        .map(|f| f.persist_to_kv_arm.clone())
        .collect();
    let persist_delta_arms: Vec<_> = field_codegens
        .iter()
        .filter_map(|f| f.persist_delta_arm.clone())
        .collect();
    let collect_valid_keys_arms: Vec<_> = field_codegens
        .iter()
        .map(|f| f.collect_valid_keys_arm.clone())
        .collect();
    let collect_valid_prefixes_arms: Vec<_> = field_codegens
        .iter()
        .filter_map(|f| f.collect_valid_prefixes_arm.clone())
        .collect();
    let all_migration_from_keys: Vec<_> = field_codegens
        .iter()
        .flat_map(|f| f.migration_from_keys.clone())
        .collect();
    let opaque_field_types: Vec<_> = field_codegens
        .iter()
        .filter_map(|f| f.opaque_field_type.clone())
        .collect();

    // Build max_value_len const expression
    let max_value_len_expr = build_const_max_expr(max_value_len_items, quote! { 0 });

    // Build MAX_KEY_LEN const expression (max of field paths)
    let max_key_len_expr = build_const_max_expr(max_key_len_items, quote! { 0 });

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

    // Build SCHEMA_HASH const
    let schema_hash_const = quote! {
        {
            let mut h = #krate::shadows::FNV1A_INIT;
            #(#schema_hash_code)*
            h
        }
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

    Ok(CodegenOutput {
        delta_type,
        reported_type,
        shadow_node_impl,
        reported_union_fields_impl,
    })
}
