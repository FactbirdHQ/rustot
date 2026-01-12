use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{punctuated::Punctuated, Data, DataEnum, DeriveInput, Fields, Ident, Token};

use crate::attr::has_default_attr;
use crate::transform::{transform_type, TypeTransformConfig, TypeWrapper};

/// Configuration for generated types
pub struct TypeDefConfig {
    /// Whether auto_derive is enabled
    pub auto_derive: bool,
    /// Whether to skip Default impl
    pub no_default: bool,
}

/// Generate all three types for shadow_patch: base (without report_only), delta, and reported
pub fn generate_shadow_patch_types(
    input: &DeriveInput,
    config: &TypeDefConfig,
) -> syn::Result<(TokenStream, Ident, Ident)> {
    let original_name = input.ident.to_string();
    let is_enum = matches!(input.data, Data::Enum(_));

    let delta_ident = format_ident!("Delta{}", &original_name);
    let reported_ident = format_ident!("Reported{}", &original_name);

    // Build derives list (only if auto_derive is enabled)
    let base_derives: Vec<&'static str> = if config.auto_derive {
        let mut derives = vec!["Serialize", "Deserialize", "Clone"];
        if !config.no_default {
            derives.push("Default");
        }
        derives
    } else {
        vec![]
    };

    // 1. Generate base type (original without report_only fields) - this is what we call "desired"
    let base_config = TypeTransformConfig {
        name: format_ident!("{}", &original_name),
        include_report_only: false,
        public_fields: false,
        type_wrapper: TypeWrapper::None,
        derives: base_derives.clone(),
        add_serde_skip: false,
        is_enum,
    };
    let base_type = transform_type(input, &base_config);
    // Use original input to find default variant, but base_type.ident for the impl
    let base_default_impl =
        generate_enum_default_impl_from_original(input, &base_type.ident, config.no_default)?;
    let base_tokens = quote! {
        #base_type
        #base_default_impl
    };

    // 2. Generate delta type
    let delta_config = TypeTransformConfig {
        name: delta_ident.clone(),
        include_report_only: false,
        public_fields: true,
        type_wrapper: TypeWrapper::OptionWithAssociated(format_ident!("Delta")),
        derives: base_derives,
        add_serde_skip: false,
        is_enum,
    };
    let delta_type = transform_type(input, &delta_config);
    let delta_default_impl =
        generate_enum_default_impl_from_original(input, &delta_type.ident, config.no_default)?;
    let delta_tokens = quote! {
        #delta_type
        #delta_default_impl
    };

    // 3. Generate reported type (includes report_only fields)
    let reported_derives = if config.auto_derive {
        vec!["Serialize", "Default"]
    } else {
        vec![]
    };
    let reported_config = TypeTransformConfig {
        name: reported_ident.clone(),
        include_report_only: true,
        public_fields: true,
        type_wrapper: TypeWrapper::OptionWithAssociated(format_ident!("Reported")),
        derives: reported_derives,
        add_serde_skip: true,
        is_enum,
    };
    let reported_type = transform_type(input, &reported_config);
    let reported_default_impl =
        generate_enum_default_impl_from_original(input, &reported_type.ident, false)?;
    let reported_tokens = quote! {
        #reported_type
        #reported_default_impl
    };

    let combined = quote! {
        #base_tokens
        #delta_tokens
        #reported_tokens
    };

    Ok((combined, delta_ident, reported_ident))
}

/// Generate Default impl for enums using the original input to find the default variant,
/// but generating the impl for the new_ident
pub fn generate_enum_default_impl(
    input: &DeriveInput,
    no_default: bool,
) -> syn::Result<TokenStream> {
    generate_enum_default_impl_from_original(input, &input.ident, no_default)
}

fn generate_enum_default_impl_from_original(
    original: &DeriveInput,
    new_ident: &Ident,
    no_default: bool,
) -> syn::Result<TokenStream> {
    if no_default {
        return Ok(quote! {});
    }

    let Data::Enum(data_enum) = &original.data else {
        return Ok(quote! {});
    };

    // Find the variant with #[default] attribute in the ORIGINAL input
    let default_variant = find_default_variant(data_enum);

    let Some(default_variant) = default_variant else {
        return Err(syn::Error::new(
            original.ident.span(),
            "Enum must have a #[default] variant",
        ));
    };

    let default_ident = &default_variant.ident;

    let default_fields = match &default_variant.fields {
        Fields::Named(fields_named) => {
            let assigners: Punctuated<TokenStream, Token![,]> = fields_named
                .named
                .iter()
                .map(|field| {
                    let field_ident = &field.ident;
                    quote! { #field_ident: Default::default() }
                })
                .collect();
            quote! { { #assigners } }
        }
        Fields::Unnamed(fields_unnamed) => {
            let assigners: Punctuated<TokenStream, Token![,]> = fields_unnamed
                .unnamed
                .iter()
                .map(|_| quote! { Default::default() })
                .collect();
            quote! { ( #assigners ) }
        }
        Fields::Unit => quote! {},
    };

    Ok(quote! {
        impl Default for #new_ident {
            fn default() -> Self {
                Self::#default_ident #default_fields
            }
        }
    })
}

fn find_default_variant(data_enum: &DataEnum) -> Option<&syn::Variant> {
    data_enum
        .variants
        .iter()
        .find(|v| has_default_attr(&v.attrs))
}
