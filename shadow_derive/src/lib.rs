extern crate proc_macro;
extern crate syn;
#[macro_use]
extern crate quote;

use proc_macro::TokenStream;
use proc_macro2::Span;
use syn::parse::{Parse, ParseStream, Parser};
use syn::parse_macro_input;
use syn::DeriveInput;
use syn::Generics;
use syn::Ident;
use syn::Result;
use syn::{parenthesized, Attribute, Error, Field, LitStr};

#[proc_macro_derive(
    ShadowState,
    attributes(shadow, unit_shadow_field, static_shadow_field)
)]
pub fn shadow_state(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as ParseInput);
    let shadow_diff = generate_shadow_diff(&input);
    let shadow_state = generate_shadow_state(&input);
    let implementation = quote! {
        #shadow_diff

        #shadow_state
    };
    TokenStream::from(implementation)
}

#[proc_macro_derive(ShadowDiff, attributes(unit_shadow_field, static_shadow_field, serde))]
pub fn shadow_diff(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as ParseInput);
    TokenStream::from(generate_shadow_diff(&input))
}

#[derive(Clone)]
struct ParseInput {
    pub ident: Ident,
    pub generics: Generics,
    pub shadow_fields: Vec<Field>,
    pub copy_attrs: Vec<Attribute>,
    pub shadow_name: Option<LitStr>,
}

impl Parse for ParseInput {
    fn parse(input: ParseStream) -> Result<Self> {
        let derive_input = DeriveInput::parse(input)?;

        let mut shadow_name = None;
        let mut copy_attrs = vec![];

        let attrs_to_copy = ["serde"];

        // Parse valid container attributes
        for attr in derive_input.attrs {
            if attr.path.is_ident("shadow") {
                fn shadow_arg(input: ParseStream) -> Result<LitStr> {
                    let content;
                    parenthesized!(content in input);
                    content.parse()
                }
                shadow_name = Some(shadow_arg.parse2(attr.tokens)?);
            } else if attrs_to_copy
                .iter()
                .find(|a| attr.path.is_ident(a))
                .is_some()
            {
                copy_attrs.push(attr);
            }
        }

        let shadow_fields = match derive_input.data {
            syn::Data::Struct(syn::DataStruct { fields, .. }) => {
                fields.into_iter().collect::<Vec<_>>()
            }
            _ => {
                return Err(Error::new(
                    Span::call_site(),
                    "ShadowState & ShadowDiff can only be implemented for non-tuple structs",
                ))
            }
        };

        Ok(Self {
            ident: derive_input.ident,
            generics: derive_input.generics,
            shadow_fields,
            copy_attrs,
            shadow_name,
        })
    }
}

fn create_assertions(fields: &Vec<Field>) -> Vec<proc_macro2::TokenStream> {
    fields
        .iter()
        .filter_map(|field| {
            if field
                .attrs
                .iter()
                .find(|a| {
                    a.path.is_ident("unit_shadow_field") || a.path.is_ident("static_shadow_field")
                })
                .is_some()
            {
                None
            } else {
                let type_name = &field.ty;
                Some(quote! { rustot::assert_impl_all!(#type_name: rustot::shadows::ShadowDiff); })
            }
        })
        .collect::<Vec<_>>()
}

fn create_assigners(fields: &Vec<Field>) -> Vec<proc_macro2::TokenStream> {
    fields
        .iter()
        .filter_map(|field| {
            let type_name = &field.ty;
            let field_name = &field.ident.clone().unwrap();

            let type_name_string = quote! {#type_name}.to_string();
            let type_name_string: String = type_name_string.chars().filter(|&c| c != ' ').collect();

            if field
                .attrs
                .iter()
                .find(|a| a.path.is_ident("static_shadow_field"))
                .is_some()
            {
                None
            } else if field
                .attrs
                .iter()
                .find(|a| a.path.is_ident("unit_shadow_field"))
                .is_some()
            {
                Some(if type_name_string.starts_with("Option<") {
                    quote! { self.#field_name = opt.#field_name; }
                } else {
                    quote! {
                        if let Some(attribute) = opt.#field_name {
                            self.#field_name = attribute;
                        }
                    }
                })
            } else {
                Some(quote! { self.#field_name.apply_patch(opt.#field_name); })
            }
        })
        .collect::<Vec<_>>()
}

fn create_optional_fields(fields: &Vec<Field>) -> Vec<proc_macro2::TokenStream> {
    fields
        .iter()
        .filter_map(|field| {
            let type_name = &field.ty;
            let attrs = field
                .attrs
                .iter()
                .filter(|a| {
                    !a.path.is_ident("static_shadow_field") && !a.path.is_ident("unit_shadow_field")
                })
                .collect::<Vec<_>>();
            let field_name = &field.ident.clone().unwrap();

            let type_name_string = quote! {#type_name}.to_string();
            let type_name_string: String = type_name_string.chars().filter(|&c| c != ' ').collect();

            if field
                .attrs
                .iter()
                .find(|a| a.path.is_ident("static_shadow_field"))
                .is_some()
            {
                None
            } else {
                Some(if type_name_string.starts_with("Option<") {
                    quote! { #(#attrs)* pub #field_name: #type_name }
                } else {
                    quote! { #(#attrs)* pub #field_name: Option<#type_name> }
                })
            }
        })
        .collect::<Vec<_>>()
}

fn generate_shadow_state(input: &ParseInput) -> proc_macro2::TokenStream {
    let ParseInput {
        ident,
        generics,
        shadow_name,
        ..
    } = input;

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let name = match shadow_name {
        Some(name) => quote! { Some(#name) },
        None => quote! { None },
    };

    return quote! {
        #[automatically_derived]
        impl #impl_generics rustot::shadows::ShadowState for #ident #ty_generics #where_clause {
            const NAME: Option<&'static str> = #name;
            // const MAX_PAYLOAD_SIZE: usize = 512;
        }
    };
}

fn generate_shadow_diff(input: &ParseInput) -> proc_macro2::TokenStream {
    let ParseInput {
        ident,
        generics,
        shadow_fields,
        copy_attrs,
        ..
    } = input;

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let optional_ident = format_ident!("Partial{}", ident);

    let assigners = create_assigners(&shadow_fields);
    let optional_fields = create_optional_fields(&shadow_fields);
    let field_assertions = create_assertions(&shadow_fields);

    return quote! {
        #[automatically_derived]
        #[derive(Default, Clone, ::serde::Deserialize, ::serde::Serialize)]
        #(#copy_attrs)*
        pub struct #optional_ident #generics {
            #(
                #optional_fields
            ),*
        }

        #(#field_assertions)*

        #[automatically_derived]
        impl #impl_generics rustot::shadows::ShadowDiff for #ident #ty_generics #where_clause {
            type PartialState = #optional_ident;

            fn apply_patch(&mut self, opt: Self::PartialState) {
                #(
                    #assigners
                )*
            }
        }
    };
}
