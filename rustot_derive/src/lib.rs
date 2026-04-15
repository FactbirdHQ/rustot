mod attr;
mod codegen;

use darling::FromMeta;
use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

#[cfg(feature = "multi")]
use attr::MultiShadowRootParams;
use attr::{FieldAttrs, ShadowNodeParams, ShadowRootParams};
use codegen::generate_shadow_node;

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
/// - `ReportedFields` implementation
/// - `desired()` / `reported()` builder methods (requires `shadows_builders` feature)
///
/// # Builder Methods
///
/// With the `shadows_builders` feature enabled, `desired()` and `reported()` builder
/// methods are generated using [`bon`]. Downstream crates must add `bon` as a direct
/// dependency in their `Cargo.toml`:
///
/// ```toml
/// [dependencies]
/// rustot = { version = "...", features = ["shadows_builders"] }
/// bon = "3"
/// ```
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
/// - `ReportedFields` implementation
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

/// The `#[multi_shadow_root(pattern = "...")]` attribute macro for multi-shadow root types.
///
/// This is the multi-shadow counterpart to [`shadow_root`]. It generates:
/// - `MultiShadowRoot` trait implementation (includes the shadow pattern)
/// - `ShadowNode` trait implementation (persistence support)
/// - `Delta{Name}` struct for applying partial updates
/// - `Reported{Name}` struct with serde skip_serializing_if
/// - `ReportedFields` implementation
/// - `desired()` / `reported()` builder methods (requires `shadows_builders` feature)
///
/// # Attributes
///
/// - `pattern = "string"` - Shadow pattern prefix (e.g., "flow-" for "flow-pump-01")
///
/// # Example
///
/// ```ignore
/// #[multi_shadow_root(pattern = "flow-")]
/// #[derive(Clone, Default, Serialize, Deserialize)]
/// struct FlowState {
///     pub flow_rate: f64,
///     pub temperature: f64,
/// }
/// ```
#[cfg(feature = "multi")]
#[proc_macro_attribute]
pub fn multi_shadow_root(
    attr: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    multi_shadow_root_impl(attr.into(), input.into())
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

    // Generate shadow node code (with ShadowRoot impl)
    let shadow_code = generate_shadow_node(&derive_input, Some(&params))?;

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

    // Generate shadow node code (without ShadowRoot impl)
    let shadow_code = generate_shadow_node(&derive_input, None)?;

    Ok(quote! {
        #original
        #shadow_code
    })
}

#[cfg(feature = "multi")]
fn multi_shadow_root_impl(attr: TokenStream, input: TokenStream) -> syn::Result<TokenStream> {
    let derive_input = syn::parse2::<DeriveInput>(input)?;

    let meta_list = darling::ast::NestedMeta::parse_meta_list(attr)
        .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), e))?;
    let params = MultiShadowRootParams::from_list(&meta_list)
        .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), e))?;

    // Strip shadow_attr from the original definition
    let original = strip_shadow_attrs(&derive_input);

    // Generate shadow node code (without ShadowRoot impl)
    let shadow_code = generate_shadow_node(&derive_input, None)?;

    // Generate MultiShadowRoot impl
    let name = &derive_input.ident;
    let krate = codegen::rustot_crate_path();
    let pattern = &params.pattern;

    let prefix_const = params.topic_prefix.as_ref().map(|p| {
        quote! { const PREFIX: &'static str = #p; }
    });

    let max_payload_const = params.max_payload_len.map(|s| {
        quote! { const MAX_PAYLOAD_SIZE: usize = #s; }
    });

    let multi_root_impl = quote! {
        impl #krate::shadows::multi::MultiShadowRoot for #name {
            const PATTERN: &'static str = #pattern;
            #prefix_const
            #max_payload_const
        }
    };

    Ok(quote! {
        #original
        #shadow_code
        #multi_root_impl
    })
}

/// Strip shadow_attr from a DeriveInput, returning clean tokens for the original definition.
///
/// Fields marked with `#[shadow_attr(report_only)]` are removed from the original struct
/// entirely — they only appear in the generated Reported type.
fn strip_shadow_attrs(input: &DeriveInput) -> TokenStream {
    let mut clean = input.clone();

    // Filter shadow_attr from type-level attributes
    clean.attrs.retain(|a| !a.path().is_ident("shadow_attr"));

    // Filter shadow_attr from field/variant attributes, and remove report_only fields
    match &mut clean.data {
        syn::Data::Struct(data) => {
            if let syn::Fields::Named(fields) = &mut data.fields {
                // Remove transient report_only fields from the original struct.
                // report_only(persist) fields remain — they need to be in the struct
                // for KV persistence and loading on boot.
                fields.named = fields
                    .named
                    .iter()
                    .filter(|field| {
                        let attrs = FieldAttrs::from_attrs(&field.attrs);
                        !attrs.is_report_only() || attrs.is_report_only_persist()
                    })
                    .cloned()
                    .collect();
                // Strip shadow_attr from remaining fields
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
