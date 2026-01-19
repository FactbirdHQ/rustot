mod attr;
mod codegen;
mod transform;
mod types;

use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

use attr::{ShadowParams, ShadowPatchParams};
use codegen::{
    generate_enum_default_impl, generate_shadow_patch_impl, generate_shadow_patch_types,
    generate_shadow_state_impl, TypeDefConfig,
};
use transform::validate_no_optional_fields;

/// The `#[shadow]` attribute macro generates AWS IoT Shadow types and implementations.
///
/// This macro generates:
/// - A `ShadowState` trait implementation for the annotated struct
/// - `Delta{Name}` struct for representing state changes
/// - `Reported{Name}` struct for representing the reported state
/// - `ShadowPatch` trait implementation with `apply_patch` and `into_reported` methods
///
/// # Attributes
///
/// - `name = "string"` - Optional shadow name for named shadows
/// - `topic_prefix = "string"` - Topic prefix (default: "$aws")
/// - `max_payload_size = N` - Maximum payload size in bytes (default: 512)
/// - `reported = TypeName` - Use a custom type for Reported instead of generating one
///
/// # Field Attributes
///
/// - `#[shadow_attr(leaf)]` - Mark field as a leaf (primitive-like, won't recursively patch)
/// - `#[shadow_attr(report_only)]` - Field only appears in Reported type, not in Delta
///
/// # Example
///
/// ```ignore
/// #[shadow(name = "device", max_payload_size = 1024)]
/// struct DeviceState {
///     pub temperature: f32,
///
///     #[shadow_attr(leaf)]
///     pub status: String,
///
///     #[shadow_attr(report_only)]
///     pub last_update: u64,
/// }
/// ```
#[proc_macro_attribute]
pub fn shadow(
    attr: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    shadow_impl(attr.into(), input.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

/// The `#[shadow_patch]` attribute macro generates Delta and Reported types
/// with the `ShadowPatch` trait implementation.
///
/// Use this macro for nested types that don't need the full `ShadowState` trait.
///
/// # Attributes
///
/// - `auto_derive = bool` - Whether to auto-derive common traits (default: true)
/// - `no_default = bool` - Skip generating Default impl for enums (default: false)
///
/// # Example
///
/// ```ignore
/// #[shadow_patch]
/// struct InnerState {
///     pub value: u32,
/// }
/// ```
#[proc_macro_attribute]
pub fn shadow_patch(
    attr: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    shadow_patch_impl(attr.into(), input.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

fn shadow_impl(attr: TokenStream, input: TokenStream) -> syn::Result<TokenStream> {
    let derive_input = syn::parse2::<DeriveInput>(input)?;
    let params = syn::parse2::<ShadowParams>(attr)?;

    // Validate the input
    validate_no_optional_fields(&derive_input)?;

    // Generate ShadowState impl
    let shadow_state_impl = generate_shadow_state_impl(&derive_input, &params);

    // Generate shadow_patch types and impl (unless custom reported is provided)
    let shadow_patch_output = if params.reported.is_some() {
        // Custom reported type - only generate delta
        generate_shadow_patch_with_custom_reported(&derive_input, &params)?
    } else {
        // Generate all types
        let patch_params = ShadowPatchParams::default();
        generate_full_shadow_patch(&derive_input, &patch_params)?
    };

    Ok(quote! {
        #shadow_state_impl
        #shadow_patch_output
    })
}

fn shadow_patch_impl(attr: TokenStream, input: TokenStream) -> syn::Result<TokenStream> {
    let derive_input = syn::parse2::<DeriveInput>(input)?;
    let params = syn::parse2::<ShadowPatchParams>(attr)?;

    // Validate the input
    validate_no_optional_fields(&derive_input)?;

    generate_full_shadow_patch(&derive_input, &params)
}

fn generate_full_shadow_patch(
    input: &DeriveInput,
    params: &ShadowPatchParams,
) -> syn::Result<TokenStream> {
    let config = TypeDefConfig {
        auto_derive: params.auto_derive,
        no_default: params.no_default,
    };

    // Generate the three types (base, delta, reported)
    let (type_defs, delta_ident, reported_ident) = generate_shadow_patch_types(input, &config)?;

    // Generate ShadowPatch impl
    let shadow_patch_impl = generate_shadow_patch_impl(input, &delta_ident, &reported_ident)?;

    Ok(quote! {
        #type_defs
        #shadow_patch_impl
    })
}

fn generate_shadow_patch_with_custom_reported(
    input: &DeriveInput,
    params: &ShadowParams,
) -> syn::Result<TokenStream> {
    use quote::format_ident;

    let original_name = input.ident.to_string();
    let delta_ident = format_ident!("Delta{}", &original_name);
    let reported_ident = params.reported.as_ref().unwrap();

    let is_enum = matches!(input.data, syn::Data::Enum(_));

    // Generate base type (the original struct with report_only fields removed)
    let base_config = transform::TypeTransformConfig {
        name: format_ident!("{}", &original_name),
        include_report_only: false,
        public_fields: false,
        type_wrapper: transform::TypeWrapper::None,
        derives: vec!["Serialize", "Deserialize", "Clone", "Default"],
        add_serde_skip: false,
        is_enum,
    };
    let base_type = transform::transform_type(input, &base_config);
    let base_default = if is_enum {
        generate_enum_default_impl(input, false)?
    } else {
        quote! {}
    };

    // Generate delta type
    let delta_config = transform::TypeTransformConfig {
        name: delta_ident.clone(),
        include_report_only: false,
        public_fields: true,
        type_wrapper: transform::TypeWrapper::OptionWithAssociated(format_ident!("Delta")),
        derives: vec!["Serialize", "Deserialize", "Clone", "Default"],
        add_serde_skip: false,
        is_enum,
    };

    let delta_type = transform::transform_type(input, &delta_config);

    // Generate enum default if needed
    let delta_default = if is_enum {
        generate_enum_default_impl(input, false)?
    } else {
        quote! {}
    };

    // Generate apply_patch body
    let apply_patch_body = generate_struct_apply_patch_body(input)?;

    // Generate ShadowPatch impl with custom reported type
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let orig_ident = &input.ident;

    let shadow_patch_impl = quote! {
        impl #impl_generics rustot::shadows::ShadowPatch for #orig_ident #ty_generics #where_clause {
            type Delta = #delta_ident #ty_generics;
            type Reported = #reported_ident #ty_generics;

            fn apply_patch(&mut self, delta: Self::Delta) {
                #apply_patch_body
            }

            fn into_reported(self) -> Self::Reported {
                self.into()
            }
        }
    };

    Ok(quote! {
        #base_type
        #base_default
        #delta_type
        #delta_default
        #shadow_patch_impl
    })
}

fn generate_struct_apply_patch_body(input: &DeriveInput) -> syn::Result<TokenStream> {
    use crate::attr::{get_attr, FieldAttrs, CFG_ATTR};
    use crate::transform::borrow_fields;
    use crate::types::is_primitive;
    use syn::{Data, Index};

    let Data::Struct(data_struct) = &input.data else {
        return Err(syn::Error::new(
            input.ident.span(),
            "Custom reported is only supported for structs",
        ));
    };

    let fields = borrow_fields(data_struct);
    let mut statements = TokenStream::new();

    for (i, field) in fields.iter().enumerate() {
        let attrs = FieldAttrs::from_attrs(&field.attrs);
        if attrs.report_only {
            continue;
        }

        let cfg_attr = get_attr(&field.attrs, CFG_ATTR)
            .map(|a| quote! { #a })
            .unwrap_or_default();

        let field_access = field
            .ident
            .as_ref()
            .map(|id| quote! { #id })
            .unwrap_or_else(|| {
                let idx = Index::from(i);
                quote! { #idx }
            });

        let is_leaf = attrs.leaf || is_primitive(&field.ty);

        let statement = if is_leaf {
            quote! {
                #cfg_attr
                if let Some(inner) = delta.#field_access {
                    self.#field_access = inner;
                }
            }
        } else {
            quote! {
                #cfg_attr
                if let Some(inner) = delta.#field_access {
                    self.#field_access.apply_patch(inner);
                }
            }
        };

        statements = quote! {
            #statements
            #statement
        };
    }

    Ok(statements)
}
