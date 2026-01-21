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
mod struct_codegen;

use proc_macro2::{Span, TokenStream};
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Ident};

use crate::attr::get_serde_tag_content;

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

/// Configuration for shadow node code generation
pub struct ShadowNodeConfig {
    /// Whether this is a root type (implements ShadowRoot)
    pub is_root: bool,
    /// The shadow name (for ShadowRoot types)
    pub name: Option<String>,
}

/// Generate all code for a shadow node type
pub fn generate_shadow_node(
    input: &DeriveInput,
    config: &ShadowNodeConfig,
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
    let (delta_type, reported_type, shadow_node_impl, reported_union_fields_impl) = if is_enum {
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
    let shadow_root_impl = if config.is_root {
        let name_value = match &config.name {
            Some(n) => quote! { Some(#n) },
            None => quote! { None },
        };

        quote! {
            impl #krate::shadows::ShadowRoot for #name {
                const NAME: Option<&'static str> = #name_value;
            }
        }
    } else {
        TokenStream::new()
    };

    Ok(quote! {
        #delta_type
        #reported_type
        #shadow_node_impl
        #shadow_root_impl
        #reported_union_fields_impl
    })
}
