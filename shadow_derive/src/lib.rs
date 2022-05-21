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

        // Parse valid container attributes
        for attr in derive_input.attrs {
            if attr.path.is_ident("shadow") {
                fn shadow_arg(input: ParseStream) -> Result<LitStr> {
                    let content;
                    parenthesized!(content in input);
                    content.parse()
                }
                shadow_name = Some(shadow_arg.parse2(attr.tokens)?);
            } else if attr.path.is_ident("serde") {
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
                    "ShadowState can only be implemented for non-tuple structs",
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

#[proc_macro_derive(ShadowState, attributes(shadow, serde))]
pub fn shadow_state(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as ParseInput);
    TokenStream::from(generate_shadow_state(&input))
}

fn create_assigners(fields: &Vec<Field>) -> Vec<proc_macro2::TokenStream> {
    fields
        .iter()
        .map(|field| {
            let ref type_name = &field.ty;
            let ref field_name = &field.ident.clone().unwrap();

            let type_name_string = quote! {#type_name}.to_string();
            let type_name_string: String = type_name_string.chars().filter(|&c| c != ' ').collect();

            if type_name_string.starts_with("Option<") {
                quote! { self.#field_name = opt.#field_name; }
            } else {
                quote! {
                    if let Some(attribute) = opt.#field_name {
                        self.#field_name = attribute;
                    }
                }
            }
        })
        .collect::<Vec<_>>()
}

fn create_optional_fields(fields: &Vec<Field>) -> Vec<proc_macro2::TokenStream> {
    fields
        .iter()
        .map(|field| {
            let type_name = &field.ty;
            let attrs = field.attrs.clone();
            let field_name = &field.ident.clone().unwrap();

            let type_name_string = quote! {#type_name}.to_string();
            let type_name_string: String = type_name_string.chars().filter(|&c| c != ' ').collect();

            if type_name_string.starts_with("Option<") {
                quote! { #(#attrs)* pub #field_name: #type_name }
            } else {
                quote! { #(#attrs)* pub #field_name: Option<#type_name> }
            }
        })
        .collect::<Vec<_>>()
}

fn generate_shadow_state(input: &ParseInput) -> proc_macro2::TokenStream {
    let ParseInput {
        ident,
        generics,
        shadow_fields,
        copy_attrs,
        shadow_name,
    } = input;

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let optional_ident = format_ident!("Optional{}", ident);

    let name = match shadow_name {
        Some(name) => quote! { Some(#name) },
        None => quote! { None },
    };

    let assigners = create_assigners(&shadow_fields);
    let optional_fields = create_optional_fields(&shadow_fields);

    return quote! {
        #[automatically_derived]
        #[derive(Default, Clone, Deserialize, Serialize)]
        #(#copy_attrs)*
        pub struct #optional_ident #generics {
            #(
                #optional_fields
            ),*
        }

        #[automatically_derived]
        impl #impl_generics rustot::shadows::ShadowState for #ident #ty_generics #where_clause {
            const NAME: Option<&'static str> = #name;
            // const MAX_PAYLOAD_SIZE: usize = 512;

            type PartialState = #optional_ident;

            fn apply_patch(&mut self, opt: Self::PartialState) {
                #(
                    #assigners
                )*
            }
        }
    };
}
