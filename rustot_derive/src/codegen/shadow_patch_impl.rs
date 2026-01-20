use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    punctuated::Punctuated, Data, DataEnum, DataStruct, DeriveInput, Field, Fields, Ident, Index,
    Token,
};

use crate::attr::{get_attr, FieldAttrs, CFG_ATTR};
use crate::transform::borrow_fields;
use crate::types::is_primitive;

/// Generate the ShadowPatch trait implementation for a struct or enum
pub fn generate_shadow_patch_impl(
    original: &DeriveInput,
    delta_ident: &Ident,
    reported_ident: &Ident,
) -> syn::Result<TokenStream> {
    let (impl_generics, ty_generics, where_clause) = original.generics.split_for_impl();
    let orig_ident = &original.ident;

    let (apply_patch_body, into_reported_body) = match &original.data {
        Data::Struct(data_struct) => {
            let apply = generate_struct_apply_patch(data_struct)?;
            let into_rep = generate_struct_into_reported(original, data_struct)?;
            (apply, into_rep)
        }
        Data::Enum(data_enum) => {
            let apply = generate_enum_apply_patch(data_enum, orig_ident)?;
            let into_rep = generate_enum_into_reported(data_enum, orig_ident)?;
            (apply, into_rep)
        }
        Data::Union(_) => {
            return Err(syn::Error::new(
                original.ident.span(),
                "Unions are not supported",
            ));
        }
    };

    Ok(quote! {
        impl #impl_generics rustot::shadows::ShadowPatch for #orig_ident #ty_generics #where_clause {
            type Delta = #delta_ident #ty_generics;
            type Reported = #reported_ident #ty_generics;

            fn apply_patch(&mut self, delta: Self::Delta) {
                #apply_patch_body
            }

            fn into_reported(self) -> Self::Reported {
                #into_reported_body
            }
        }
    })
}

/// Generate apply_patch body for structs
fn generate_struct_apply_patch(data_struct: &DataStruct) -> syn::Result<TokenStream> {
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

        let is_leaf = attrs.opaque || is_primitive(&field.ty);

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

/// Generate into_reported body for structs
fn generate_struct_into_reported(
    _original: &DeriveInput,
    data_struct: &DataStruct,
) -> syn::Result<TokenStream> {
    let fields = borrow_fields(data_struct);
    let mut field_inits = TokenStream::new();

    for field in fields.iter() {
        let attrs = FieldAttrs::from_attrs(&field.attrs);
        let cfg_attr = get_attr(&field.attrs, CFG_ATTR)
            .map(|a| quote! { #a })
            .unwrap_or_default();

        let ident = field.ident.as_ref().expect("Struct fields must be named");
        let is_leaf = attrs.opaque || is_primitive(&field.ty);

        let init = if attrs.report_only {
            // report_only fields are initialized to None
            quote! { #cfg_attr #ident: None, }
        } else if is_leaf {
            quote! { #cfg_attr #ident: Some(self.#ident), }
        } else {
            quote! { #cfg_attr #ident: Some(self.#ident.into_reported()), }
        };

        field_inits = quote! {
            #field_inits
            #init
        };
    }

    Ok(quote! {
        Self::Reported {
            #field_inits
        }
    })
}

/// Generate apply_patch body for enums
fn generate_enum_apply_patch(
    data_enum: &DataEnum,
    _orig_ident: &Ident,
) -> syn::Result<TokenStream> {
    let mut arms = TokenStream::new();

    for variant in &data_enum.variants {
        let variant_ident = &variant.ident;
        let cfg_attr = get_attr(&variant.attrs, CFG_ATTR)
            .map(|a| quote! { #a })
            .unwrap_or_default();

        let variant_arms = match &variant.fields {
            Fields::Named(fields_named) => {
                let patch = PatchImpl::from_fields(fields_named.named.iter());
                let variables = &patch.variables;
                let delta_deconstructors = &patch.delta_deconstructors;
                let assigns = &patch.assigns;
                let defaults = &patch.defaults;

                quote! {
                    #cfg_attr
                    (Self::#variant_ident { #(#variables),* }, Self::Delta::#variant_ident { #(#delta_deconstructors),* }) => {
                        #(#assigns)*
                    }
                    #cfg_attr
                    (this, Self::Delta::#variant_ident { #(#delta_deconstructors),* }) => {
                        #(#defaults)*
                        *this = Self::#variant_ident { #(#variables),* };
                    }
                }
            }
            Fields::Unnamed(fields_unnamed) => {
                let patch = PatchImpl::from_fields(fields_unnamed.unnamed.iter());
                let variables = &patch.variables;
                let delta_variables = &patch.delta_variables;
                let assigns = &patch.assigns;
                let defaults = &patch.defaults;

                quote! {
                    #cfg_attr
                    (Self::#variant_ident ( #(#variables),* ), Self::Delta::#variant_ident ( #(#delta_variables),* )) => {
                        #(#assigns)*
                    }
                    #cfg_attr
                    (this, Self::Delta::#variant_ident ( #(#delta_variables),* )) => {
                        #(#defaults)*
                        *this = Self::#variant_ident ( #(#variables),* );
                    }
                }
            }
            Fields::Unit => {
                quote! {
                    #cfg_attr
                    (this, Self::Delta::#variant_ident) => *this = Self::#variant_ident,
                }
            }
        };

        arms = quote! {
            #arms
            #variant_arms
        };
    }

    Ok(quote! {
        match (self, delta) {
            #arms
        }
    })
}

/// Generate into_reported body for enums
fn generate_enum_into_reported(
    data_enum: &DataEnum,
    orig_ident: &Ident,
) -> syn::Result<TokenStream> {
    let mut arms: Punctuated<TokenStream, Token![,]> = Punctuated::new();

    for variant in &data_enum.variants {
        let variant_ident = &variant.ident;
        let cfg_attr = get_attr(&variant.attrs, CFG_ATTR)
            .map(|a| quote! { #a })
            .unwrap_or_default();

        let arm = match &variant.fields {
            Fields::Named(fields_named) => {
                let (variables, actions) = variables_and_actions(fields_named.named.iter());
                quote! {
                    #cfg_attr #orig_ident::#variant_ident { #variables } => Self::Reported::#variant_ident { #actions }
                }
            }
            Fields::Unnamed(fields_unnamed) => {
                let (variables, actions) = variables_and_actions(fields_unnamed.unnamed.iter());
                quote! {
                    #cfg_attr #orig_ident::#variant_ident ( #variables ) => Self::Reported::#variant_ident ( #actions )
                }
            }
            Fields::Unit => {
                quote! {
                    #cfg_attr #orig_ident::#variant_ident => Self::Reported::#variant_ident
                }
            }
        };

        arms.push(arm);
    }

    Ok(quote! {
        match self {
            #arms
        }
    })
}

/// Helper to generate variable names and their reported transformations
fn variables_and_actions<'a>(
    fields: impl Iterator<Item = &'a Field>,
) -> (
    Punctuated<Ident, Token![,]>,
    Punctuated<TokenStream, Token![,]>,
) {
    fields.enumerate().fold(
        (Punctuated::new(), Punctuated::new()),
        |(mut variables, mut actions), (i, field)| {
            let var_ident = field
                .ident
                .clone()
                .unwrap_or_else(|| format_ident!("{}", (b'a' + i as u8) as char));

            let attrs = FieldAttrs::from_attrs(&field.attrs);
            let is_leaf = attrs.opaque || is_primitive(&field.ty);

            let action = if is_leaf {
                quote! { Some(#var_ident) }
            } else {
                quote! { Some(#var_ident.into_reported()) }
            };

            actions.push(
                field
                    .ident
                    .as_ref()
                    .map(|ident| quote! { #ident: #action })
                    .unwrap_or(action),
            );

            variables.push(var_ident);

            (variables, actions)
        },
    )
}

/// Helper struct for generating patch implementation code
struct PatchImpl {
    variables: Vec<TokenStream>,
    delta_variables: Vec<TokenStream>,
    delta_deconstructors: Vec<TokenStream>,
    defaults: Vec<TokenStream>,
    assigns: Vec<TokenStream>,
}

impl PatchImpl {
    fn from_fields<'a>(fields: impl Iterator<Item = &'a Field>) -> Self {
        let mut result = Self {
            variables: Vec::new(),
            delta_variables: Vec::new(),
            delta_deconstructors: Vec::new(),
            defaults: Vec::new(),
            assigns: Vec::new(),
        };

        for (i, field) in fields.enumerate() {
            let cfg_attr = get_attr(&field.attrs, CFG_ATTR)
                .map(|a| quote! { #a })
                .unwrap_or_default();

            let var_ident = field
                .ident
                .clone()
                .unwrap_or_else(|| format_ident!("{}", (b'a' + i as u8) as char));

            let delta_ident = format_ident!("d_{}", var_ident);

            let cfg_var = quote! { #cfg_attr #var_ident };
            let cfg_delta = quote! { #cfg_attr #delta_ident };

            let attrs = FieldAttrs::from_attrs(&field.attrs);
            let field_ty = &field.ty;
            let is_leaf = attrs.opaque || is_primitive(field_ty);

            let (default_action, assign_action) = if is_leaf {
                (
                    quote! {
                        #cfg_attr
                        let #var_ident = #delta_ident.unwrap_or_default();
                    },
                    quote! {
                        #cfg_attr
                        if let Some(delta_var) = #delta_ident {
                            *#var_ident = delta_var;
                        }
                    },
                )
            } else {
                (
                    quote! {
                        #cfg_attr
                        let mut #var_ident = #field_ty::default();
                        #cfg_attr
                        if let Some(delta_var) = #delta_ident {
                            #var_ident.apply_patch(delta_var);
                        }
                    },
                    quote! {
                        #cfg_attr
                        if let Some(delta_var) = #delta_ident {
                            #var_ident.apply_patch(delta_var);
                        }
                    },
                )
            };

            result.defaults.push(default_action);
            result.assigns.push(assign_action);
            result.variables.push(cfg_var);
            result.delta_variables.push(cfg_delta);
            result
                .delta_deconstructors
                .push(quote! { #var_ident: #delta_ident });
        }

        result
    }
}
