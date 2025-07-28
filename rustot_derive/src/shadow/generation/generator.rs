use proc_macro2::TokenStream;
use quote::quote;
use syn::{punctuated::Punctuated, Data, DeriveInput, Token};

use crate::shadow::{generation::variant_or_field_visitor::get_attr, DEFAULT_ATTRIBUTE};

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

pub struct DefaultGenerator(pub bool);

impl Generator for DefaultGenerator {
    fn generate(&mut self, _original: &DeriveInput, output: &DeriveInput) -> TokenStream {
        if self.0 {
            return quote! {};
        }

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
