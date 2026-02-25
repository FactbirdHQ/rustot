//! Code generation for ShadowNode and ShadowRoot traits
//!
//! This module generates:
//! - Delta type for applying partial updates
//! - Reported type with skip_serializing_if
//! - ShadowNode trait implementation
//! - ShadowRoot trait implementation (for root types)
//! - ReportedUnionFields implementation

mod adjacently_tagged;
mod enum_codegen;
#[cfg(feature = "kv_persist")]
mod helpers;
#[cfg(feature = "kv_persist")]
mod kv_codegen;
mod struct_codegen;

use proc_macro2::{Span, TokenStream};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Ident};

use crate::attr::{get_serde_tag_content, ShadowRootParams};

/// Output of code generation for a struct or enum type.
///
/// This struct captures all the generated code fragments that need to be
/// emitted together with the original type definition.
pub(crate) struct CodegenOutput {
    /// The Delta type definition (e.g., `struct DeltaFoo { ... }`)
    pub delta_type: TokenStream,
    /// The Reported type definition (e.g., `struct ReportedFoo { ... }`)
    pub reported_type: TokenStream,
    /// The `ShadowNode` trait implementation (includes `KVPersist` when feature-enabled)
    pub shadow_node_impl: TokenStream,
    /// The `ReportedUnionFields` trait implementation
    pub reported_union_fields_impl: TokenStream,
}

/// Get the path to the rustot crate, handling both internal and external usage.
pub(crate) fn rustot_crate_path() -> TokenStream {
    match crate_name("rustot") {
        Ok(FoundCrate::Itself) => quote!(crate),
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name, Span::call_site());
            quote!(::#ident)
        }
        Err(_) => {
            // Fallback to ::rustot if not found (shouldn't happen in normal usage)
            quote!(::rustot)
        }
    }
}

/// Generate all code for a shadow node type.
///
/// # Arguments
///
/// * `input` - The parsed derive input
/// * `root_params` - If `Some`, this is a root type and will implement `ShadowRoot`.
///   If `None`, only `ShadowNode` is implemented.
pub fn generate_shadow_node(
    input: &DeriveInput,
    root_params: Option<&ShadowRootParams>,
) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let delta_name = format_ident!("Delta{}", name);
    let reported_name = format_ident!("Reported{}", name);
    let krate = rustot_crate_path();

    // Check if this is an enum
    let is_enum = matches!(&input.data, Data::Enum(_));

    // Check for adjacently-tagged enum
    let (tag_key, content_key) = get_serde_tag_content(&input.attrs);
    let is_adjacently_tagged = tag_key.is_some() && content_key.is_some();

    // Generate code based on struct vs enum
    let output = if is_enum {
        enum_codegen::generate_enum_code(
            input,
            &delta_name,
            &reported_name,
            is_adjacently_tagged,
            &krate,
        )?
    } else {
        struct_codegen::generate_struct_code(input, &delta_name, &reported_name, &krate)?
    };

    // Generate ShadowRoot impl if this is a root type
    let shadow_root_impl = if let Some(params) = root_params {
        let name_value = match &params.name {
            Some(n) => quote! { Some(#n) },
            None => quote! { None },
        };

        let prefix_const = params.topic_prefix.as_ref().map(|p| {
            quote! { const PREFIX: &'static str = #p; }
        });

        let max_payload_const = params.max_payload_len.map(|s| {
            quote! { const MAX_PAYLOAD_SIZE: usize = #s; }
        });

        quote! {
            impl #krate::shadows::ShadowRoot for #name {
                const NAME: Option<&'static str> = #name_value;
                #prefix_const
                #max_payload_const
            }
        }
    } else {
        TokenStream::new()
    };

    let delta_type = output.delta_type;
    let reported_type = output.reported_type;
    let shadow_node_impl = output.shadow_node_impl;
    let reported_union_fields_impl = output.reported_union_fields_impl;

    Ok(quote! {
        #delta_type
        #reported_type
        #shadow_node_impl
        #shadow_root_impl
        #reported_union_fields_impl
    })
}
