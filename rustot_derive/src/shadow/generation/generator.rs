use proc_macro2::TokenStream;
use quote::quote;
use syn::{punctuated::Punctuated, spanned::Spanned as _, Data, DeriveInput, Token};

use crate::shadow::{
    generation::variant_or_field_visitor::{borrow_fields, get_attr, has_shadow_arg, is_primitive},
    CFG_ATTRIBUTE, DEFAULT_ATTRIBUTE,
};

pub trait Generator {
    fn generate(&mut self, original: &DeriveInput, output: &DeriveInput) -> TokenStream;
}

pub struct NewGenerator;

impl Generator for NewGenerator {
    fn generate(&mut self, _original: &DeriveInput, output: &DeriveInput) -> TokenStream {
        quote! {
            #output
        }
    }
}

pub struct GenerateFromImpl;

impl GenerateFromImpl {
    fn variables_actions<'a>(
        fields: impl Iterator<Item = &'a syn::Field>,
    ) -> (
        Punctuated<syn::Ident, Token![,]>,
        Punctuated<TokenStream, Token![,]>,
    ) {
        fields.enumerate().fold(
            (Punctuated::new(), Punctuated::new()),
            |(mut variables, mut actions), (i, field)| {
                let var_ident = field.ident.clone().unwrap_or_else(|| {
                    syn::Ident::new(&format!("{}", (b'a' + i as u8) as char), field.span())
                });

                let action = if is_primitive(&field.ty) || has_shadow_arg(&field, "leaf") {
                    quote! {Some(#var_ident)}
                } else {
                    quote! {Some(#var_ident.into())}
                };

                actions.push(
                    field
                        .ident
                        .as_ref()
                        .map(|ident| quote! {#ident: #action})
                        .unwrap_or(action),
                );

                variables.push(var_ident);

                (variables, actions)
            },
        )
    }
}

impl Generator for GenerateFromImpl {
    fn generate(&mut self, original: &DeriveInput, output: &DeriveInput) -> TokenStream {
        let (impl_generics, ty_generics, _) = original.generics.split_for_impl();
        let orig_name = &original.ident;
        let new_name = &output.ident;

        let from_impl = match (&original.data, &output.data) {
            (Data::Struct(data_struct_old), Data::Struct(data_struct_new)) => {
                let original_fields = borrow_fields(data_struct_old);
                let new_fields = borrow_fields(data_struct_new);

                let from_fields = new_fields.iter().fold(quote! {}, |acc, field| {
                    let is_leaf = original_fields
                        .iter()
                        .find(|&f| f.ident == field.ident)
                        .map(|f| is_primitive(&f.ty) || has_shadow_arg(&f, "leaf"))
                        .unwrap_or_default();

                    let cfg_attr = get_attr(&field.attrs, CFG_ATTRIBUTE);

                    let ident = &field.ident;
                    if is_leaf {
                        quote! { #acc #cfg_attr #ident: Some(v.#ident), }
                    } else {
                        quote! { #acc #cfg_attr #ident: Some(v.#ident.into()), }
                    }
                });

                quote! {
                    Self {
                        #from_fields
                        ..Default::default()
                    }
                }
            }
            (Data::Enum(data_struct_old), Data::Enum(_)) => {
                let match_arms = data_struct_old
                    .variants
                    .iter()
                    .fold(Punctuated::<TokenStream, Token![,]>::new(), |mut acc, variant| {
                        let variant_ident = &variant.ident;
                        let cfg_attr = get_attr(&variant.attrs, CFG_ATTRIBUTE);

                        acc.push(match &variant.fields {
                            syn::Fields::Named(fields_named) => {
                                let (variables, actions) = Self::variables_actions(fields_named.named.iter());
                                quote! {#cfg_attr #orig_name::#variant_ident { #variables } => Self::#variant_ident { #actions }}
                            }
                            syn::Fields::Unnamed(fields_unnamed) => {
                                let (variables, actions) = Self::variables_actions(fields_unnamed.unnamed.iter());
                                quote! {#cfg_attr #orig_name::#variant_ident ( #variables ) => Self::#variant_ident ( #actions )}
                            }
                            syn::Fields::Unit => {
                                quote! {#cfg_attr #orig_name::#variant_ident => Self::#variant_ident}
                            }
                        });

                        acc
                    });

                quote! {
                    match v {
                        #match_arms
                    }
                }
            }
            _ => panic!(),
        };

        quote! {
            impl #impl_generics From<#orig_name #ty_generics> for #new_name #ty_generics {
                fn from(v: #orig_name #ty_generics) -> Self {
                    #from_impl
                }
            }
        }
    }
}

pub struct DefaultGenerator;

impl Generator for DefaultGenerator {
    fn generate(&mut self, _original: &DeriveInput, output: &DeriveInput) -> TokenStream {
        if let Data::Enum(enum_data) = &output.data {
            let default_variant = enum_data
                .variants
                .iter()
                .find(|variant| get_attr(&variant.attrs, DEFAULT_ATTRIBUTE).is_some())
                .expect("");

            let default_ident = &default_variant.ident;

            let default_fields = match &default_variant.fields {
                syn::Fields::Named(fields_named) => {
                    let assigners = fields_named.named.iter().fold(
                        Punctuated::<TokenStream, Token![,]>::new(),
                        |mut acc, field| {
                            let ident = &field.ident;
                            acc.push(quote! { #ident: Default::default() });
                            acc
                        },
                    );
                    quote! { { #assigners } }
                }
                syn::Fields::Unnamed(fields_unnamed) => {
                    let assigners = fields_unnamed.unnamed.iter().fold(
                        Punctuated::<TokenStream, Token![,]>::new(),
                        |mut acc, _field| {
                            acc.push(quote! { Default::default() });
                            acc
                        },
                    );
                    quote! { ( #assigners ) }
                }
                syn::Fields::Unit => quote! {},
            };

            let ident = &output.ident;

            return quote! {
                impl Default for #ident {
                    fn default() -> Self {
                        Self::#default_ident #default_fields
                    }
                }
            };
        }
        quote! {}
    }
}
