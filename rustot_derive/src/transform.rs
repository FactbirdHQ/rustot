use syn::{
    parse_quote, punctuated::Punctuated, spanned::Spanned, Attribute, Data, DataStruct,
    DeriveInput, Field, Fields, Ident, Path, Token, Visibility,
};

use crate::attr::{FieldAttrs, SHADOW_ATTR};
use crate::types::{extract_inner_from_option, is_primitive};

/// How to wrap the field type
#[derive(Debug, Clone)]
pub enum TypeWrapper {
    /// Keep the type as-is
    None,
    /// Wrap in Option<T::Associated> (e.g., Option<T::Delta>)
    OptionWithAssociated(Ident),
}

/// Configuration for transforming a type definition
#[derive(Debug, Clone)]
pub struct TypeTransformConfig {
    /// New name for the type (e.g., "DeltaFoo")
    pub name: Ident,
    /// Whether to include report_only fields
    pub include_report_only: bool,
    /// Whether to make fields public
    pub public_fields: bool,
    /// How to wrap field types
    pub type_wrapper: TypeWrapper,
    /// Derives to add
    pub derives: Vec<&'static str>,
    /// Whether to add serde skip_serializing_if attribute
    pub add_serde_skip: bool,
    /// Whether this is for an enum (affects derive handling)
    pub is_enum: bool,
}

/// Transform a DeriveInput according to the config, producing a new struct/enum definition
pub fn transform_type(input: &DeriveInput, config: &TypeTransformConfig) -> DeriveInput {
    let mut output = input.clone();

    // Rename the type
    output.ident = config.name.clone();

    // Update derives
    update_derives(&mut output.attrs, config);

    // Transform the data (struct fields or enum variants)
    match &mut output.data {
        Data::Struct(data_struct) => {
            transform_struct_fields(data_struct, config);
        }
        Data::Enum(data_enum) => {
            for variant in data_enum.variants.iter_mut() {
                transform_variant_fields(&mut variant.fields, config);
                // Remove shadow attributes from variant
                remove_shadow_attrs(&mut variant.attrs);
            }
        }
        Data::Union(_) => panic!("Unions are not supported"),
    }

    output
}

/// Validate that non-report_only fields are not Option types
pub fn validate_no_optional_fields(input: &DeriveInput) -> syn::Result<()> {
    if let Data::Struct(data_struct) = &input.data {
        for field in borrow_fields(data_struct) {
            let attrs = FieldAttrs::from_attrs(&field.attrs);
            if !attrs.report_only && extract_inner_from_option(&field.ty).is_some() {
                return Err(syn::Error::new(
                    field.ty.span(),
                    "Optional fields are only allowed with #[shadow_attr(report_only)]",
                ));
            }
        }
    }
    Ok(())
}

fn transform_struct_fields(data_struct: &mut DataStruct, config: &TypeTransformConfig) {
    let fields = borrow_fields_mut(data_struct);

    // Filter out report_only fields if needed
    if !config.include_report_only {
        let filtered: Punctuated<Field, Token![,]> = fields
            .iter()
            .filter(|f| !FieldAttrs::from_attrs(&f.attrs).report_only)
            .cloned()
            .collect();
        *fields = filtered;
    }

    // Transform remaining fields
    for field in fields.iter_mut() {
        transform_field(field, config);
    }
}

fn transform_variant_fields(fields: &mut Fields, config: &TypeTransformConfig) {
    // Create a modified config that doesn't set visibility (enum variants share enum visibility)
    let mut variant_config = config.clone();
    variant_config.public_fields = false;

    match fields {
        Fields::Named(named) => {
            for field in named.named.iter_mut() {
                transform_field(field, &variant_config);
            }
        }
        Fields::Unnamed(unnamed) => {
            for field in unnamed.unnamed.iter_mut() {
                transform_field(field, &variant_config);
            }
        }
        Fields::Unit => {}
    }
}

fn transform_field(field: &mut Field, config: &TypeTransformConfig) {
    let attrs = FieldAttrs::from_attrs(&field.attrs);

    // Set visibility
    if config.public_fields {
        field.vis = Visibility::Public(syn::token::Pub(field.vis.span()));
    }

    // Transform type
    let original_ty = &field.ty;
    let is_leaf = attrs.leaf || is_primitive(original_ty);

    // Check if the original type is already an Option
    let (inner_ty, is_base_opt) = match extract_inner_from_option(original_ty) {
        Some(inner) => (inner, true),
        None => (original_ty, false),
    };

    // Determine if the inner type is a leaf
    let inner_is_leaf = is_leaf || (is_base_opt && (attrs.leaf || is_primitive(inner_ty)));

    field.ty = match &config.type_wrapper {
        TypeWrapper::None => original_ty.clone(),
        TypeWrapper::OptionWithAssociated(assoc) => {
            if inner_is_leaf {
                parse_quote!(Option<#original_ty>)
            } else if is_base_opt {
                parse_quote!(Option<Option<<#inner_ty as rustot::shadows::ShadowPatch>::#assoc>>)
            } else {
                parse_quote!(Option<<#original_ty as rustot::shadows::ShadowPatch>::#assoc>)
            }
        }
    };

    // Add serde skip attribute if needed
    if config.add_serde_skip {
        field
            .attrs
            .push(parse_quote!(#[serde(skip_serializing_if = "Option::is_none")]));
    }

    // Remove shadow attributes
    remove_shadow_attrs(&mut field.attrs);
}

fn remove_shadow_attrs(attrs: &mut Vec<Attribute>) {
    attrs.retain(|attr| !attr.path().is_ident(SHADOW_ATTR) && !attr.path().is_ident("default"));
}

fn update_derives(attrs: &mut Vec<Attribute>, config: &TypeTransformConfig) {
    // Collect existing derives (excluding our auto-derives and Default for enums)
    let mut all_derives: Punctuated<Path, Token![,]> = Punctuated::new();

    // Add configured derives first
    for derive in &config.derives {
        // Skip Default for enums unless no_default is false
        if config.is_enum && *derive == "Default" {
            continue;
        }
        all_derives.push(Path::from(Ident::new(derive, proc_macro2::Span::call_site())));
    }

    // Preserve existing derives that aren't in our list
    attrs.retain(|attr| {
        if attr.path().is_ident("derive") {
            if let Ok(derives) =
                attr.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)
            {
                for derive in derives {
                    // Skip Default for enums
                    if config.is_enum && derive.is_ident("Default") {
                        continue;
                    }
                    // Add if not already present
                    if !all_derives.iter().any(|d| paths_equal(d, &derive)) {
                        all_derives.push(derive);
                    }
                }
            }
            false // Remove the original derive attribute
        } else {
            true
        }
    });

    // Insert new derive attribute at the beginning
    if !all_derives.is_empty() {
        attrs.insert(0, parse_quote!(#[derive(#all_derives)]));
    }
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    if a.segments.len() != b.segments.len() {
        return false;
    }
    a.segments
        .iter()
        .zip(b.segments.iter())
        .all(|(a, b)| a.ident == b.ident)
}

/// Borrow fields from a DataStruct
pub fn borrow_fields(data_struct: &DataStruct) -> &Punctuated<Field, Token![,]> {
    match &data_struct.fields {
        Fields::Named(f) => &f.named,
        Fields::Unnamed(f) => &f.unnamed,
        Fields::Unit => panic!("Unit structs are not supported"),
    }
}

/// Borrow fields mutably from a DataStruct
pub fn borrow_fields_mut(data_struct: &mut DataStruct) -> &mut Punctuated<Field, Token![,]> {
    match &mut data_struct.fields {
        Fields::Named(f) => &mut f.named,
        Fields::Unnamed(f) => &mut f.unnamed,
        Fields::Unit => panic!("Unit structs are not supported"),
    }
}
