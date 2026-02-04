mod attr;
mod codegen;

use darling::FromMeta;
use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

use attr::{ShadowNodeParams, ShadowRootParams};
use codegen::{generate_shadow_node, ShadowNodeConfig};

// =============================================================================
// KV-based shadow macros (Phase 8)
// =============================================================================

/// The `#[shadow_root(name = "...")]` attribute macro for top-level shadow types.
///
/// This macro generates:
/// - `ShadowRoot` trait implementation (includes the shadow name)
/// - `ShadowNode` trait implementation (persistence support)
/// - `Delta{Name}` struct for applying partial updates
/// - `Reported{Name}` struct with serde skip_serializing_if
/// - `ReportedUnionFields` implementation
///
/// # Attributes
///
/// - `name = "string"` - Shadow name for KV key prefix
///
/// # Field Attributes
///
/// - `#[shadow_attr(opaque)]` - Mark field as opaque (primitive-like, won't recursively patch)
/// - `#[shadow_attr(report_only)]` - Field only appears in Reported type
/// - `#[shadow_attr(migrate(from = "old_key"))]` - Migration from old key
/// - `#[shadow_attr(migrate(from = "old_key", convert = fn))]` - Migration with conversion
/// - `#[shadow_attr(default = value)]` - Custom default value
///
/// # Example
///
/// ```ignore
/// #[shadow_root(name = "device")]
/// #[derive(Clone, Default)]
/// struct DeviceShadow {
///     pub config: Config,
///     pub version: u32,
/// }
/// ```
#[proc_macro_attribute]
pub fn shadow_root(
    attr: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    shadow_root_impl(attr.into(), input.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

/// The `#[shadow_node]` attribute macro for nested shadow types.
///
/// This macro generates:
/// - `ShadowNode` trait implementation (persistence support)
/// - `Delta{Name}` struct/enum for applying partial updates
/// - `Reported{Name}` struct/enum with serde skip_serializing_if
/// - `ReportedUnionFields` implementation
///
/// # Field Attributes
///
/// - `#[shadow_attr(opaque)]` - Mark field as opaque (primitive-like, won't recursively patch)
/// - `#[shadow_attr(report_only)]` - Field only appears in Reported type
/// - `#[shadow_attr(migrate(from = "old_key"))]` - Migration from old key
/// - `#[shadow_attr(default = value)]` - Custom default value
///
/// # Supported Types
///
/// - Structs with named fields
/// - Enums with unit or newtype variants
///
/// # Example
///
/// ```ignore
/// #[shadow_node]
/// #[derive(Clone, Default)]
/// struct Config {
///     pub timeout: u32,
///     pub retries: u8,
/// }
///
/// #[shadow_node]
/// #[derive(Clone)]
/// enum IpSettings {
///     #[default]
///     Dhcp,
///     Static(StaticConfig),
/// }
/// ```
#[proc_macro_attribute]
pub fn shadow_node(
    attr: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    shadow_node_impl(attr.into(), input.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

fn shadow_root_impl(attr: TokenStream, input: TokenStream) -> syn::Result<TokenStream> {
    let derive_input = syn::parse2::<DeriveInput>(input)?;

    // Parse attributes using Darling
    let params = if attr.is_empty() {
        ShadowRootParams::default()
    } else {
        let meta_list = darling::ast::NestedMeta::parse_meta_list(attr)
            .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), e))?;
        ShadowRootParams::from_list(&meta_list)
            .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), e))?
    };

    // Strip shadow_attr from the original definition
    let original = strip_shadow_attrs(&derive_input);

    // Generate shadow node code
    let config = ShadowNodeConfig {
        is_root: true,
        name: params.name,
        topic_prefix: params.topic_prefix,
        max_payload_len: params.max_payload_len,
    };

    let shadow_code = generate_shadow_node(&derive_input, &config)?;

    Ok(quote! {
        #original
        #shadow_code
    })
}

fn shadow_node_impl(attr: TokenStream, input: TokenStream) -> syn::Result<TokenStream> {
    let derive_input = syn::parse2::<DeriveInput>(input)?;

    // Parse attributes using Darling (currently no params, but validates no unknown attrs)
    if !attr.is_empty() {
        let meta_list = darling::ast::NestedMeta::parse_meta_list(attr)
            .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), e))?;
        let _params = ShadowNodeParams::from_list(&meta_list)
            .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), e))?;
    }

    // Strip shadow_attr from the original definition
    let original = strip_shadow_attrs(&derive_input);

    // Generate shadow node code
    let config = ShadowNodeConfig {
        is_root: false,
        name: None,
        topic_prefix: None,
        max_payload_len: None,
    };

    let shadow_code = generate_shadow_node(&derive_input, &config)?;

    Ok(quote! {
        #original
        #shadow_code
    })
}

/// Strip shadow_attr from a DeriveInput, returning clean tokens for the original definition
fn strip_shadow_attrs(input: &DeriveInput) -> TokenStream {
    let mut clean = input.clone();

    // Filter shadow_attr from type-level attributes
    clean.attrs.retain(|a| !a.path().is_ident("shadow_attr"));

    // Filter shadow_attr from field/variant attributes
    match &mut clean.data {
        syn::Data::Struct(data) => {
            if let syn::Fields::Named(fields) = &mut data.fields {
                for field in &mut fields.named {
                    field.attrs.retain(|a| !a.path().is_ident("shadow_attr"));
                }
            } else if let syn::Fields::Unnamed(fields) = &mut data.fields {
                for field in &mut fields.unnamed {
                    field.attrs.retain(|a| !a.path().is_ident("shadow_attr"));
                }
            }
        }
        syn::Data::Enum(data) => {
            for variant in &mut data.variants {
                variant.attrs.retain(|a| !a.path().is_ident("shadow_attr"));
                match &mut variant.fields {
                    syn::Fields::Named(fields) => {
                        for field in &mut fields.named {
                            field.attrs.retain(|a| !a.path().is_ident("shadow_attr"));
                        }
                    }
                    syn::Fields::Unnamed(fields) => {
                        for field in &mut fields.unnamed {
                            field.attrs.retain(|a| !a.path().is_ident("shadow_attr"));
                        }
                    }
                    syn::Fields::Unit => {}
                }
            }
        }
        syn::Data::Union(_) => {}
    }

    quote! { #clean }
}
