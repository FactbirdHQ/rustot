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
use syn::{parenthesized, Error, Field, LitStr};

#[proc_macro_derive(ShadowState, attributes(shadow, static_shadow_field, patch))]
pub fn shadow_state(input: TokenStream) -> TokenStream {
    match parse_macro_input!(input as ParseInput) {
        ParseInput::Struct(input) => {
            let shadow_patch = generate_shadow_patch_struct(&input);
            let shadow_state = generate_shadow_state(&input);
            let implementation = quote! {
                #shadow_patch

                #shadow_state
            };
            TokenStream::from(implementation)
        }
        _ => {
            todo!()
        }
    }
}

#[proc_macro_derive(ShadowPatch, attributes(static_shadow_field, patch))]
pub fn shadow_patch(input: TokenStream) -> TokenStream {
    TokenStream::from(match parse_macro_input!(input as ParseInput) {
        ParseInput::Struct(input) => generate_shadow_patch_struct(&input),
        ParseInput::Enum(input) => generate_shadow_patch_enum(&input),
    })
}

enum ParseInput {
    Struct(StructParseInput),
    Enum(EnumParseInput),
}

#[derive(Clone)]
struct EnumParseInput {
    pub ident: Ident,
    pub generics: Generics,
}

#[derive(Clone)]
struct StructParseInput {
    pub ident: Ident,
    pub generics: Generics,
    pub shadow_fields: Vec<Field>,
    pub copy_attrs: Vec<proc_macro2::TokenStream>,
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
            } else if attr.path.is_ident("patch") {
                fn patch_arg(input: ParseStream) -> Result<proc_macro2::TokenStream> {
                    let content;
                    parenthesized!(content in input);
                    content.parse()
                }
                let args = patch_arg.parse2(attr.tokens)?;
                copy_attrs.push(quote! { #[ #args ]})
            }
        }

        match derive_input.data {
            syn::Data::Struct(syn::DataStruct { fields, .. }) => {
                Ok(Self::Struct(StructParseInput {
                    ident: derive_input.ident,
                    generics: derive_input.generics,
                    shadow_fields: fields.into_iter().collect::<Vec<_>>(),
                    copy_attrs,
                    shadow_name,
                }))
            }
            syn::Data::Enum(syn::DataEnum { .. }) => Ok(Self::Enum(EnumParseInput {
                ident: derive_input.ident,
                generics: derive_input.generics,
            })),
            _ => Err(Error::new(
                Span::call_site(),
                "ShadowState & ShadowPatch can only be derived for non-tuple structs & enums",
            )),
        }
    }
}

fn create_assigners(fields: &Vec<Field>) -> Vec<proc_macro2::TokenStream> {
    fields
        .iter()
        .filter_map(|field| {
            let field_name = &field.ident.clone().unwrap();

            if field
                .attrs
                .iter()
                .find(|a| a.path.is_ident("static_shadow_field"))
                .is_some()
            {
                None
            } else {
                Some(quote! {
                    if let Some(attribute) = opt.#field_name {
                        self.#field_name.apply_patch(attribute);
                    }
                })
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
                    !a.path.is_ident("static_shadow_field")
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
                    quote! { #(#attrs)* pub #field_name: Option<rustot::shadows::Patch<<#type_name as rustot::shadows::ShadowPatch>::PatchState>> }
                } else {
                    quote! { #(#attrs)* #[serde(skip_serializing_if = "Option::is_none")] pub #field_name: Option<<#type_name as rustot::shadows::ShadowPatch>::PatchState> }
                })
            }
        })
        .collect::<Vec<_>>()
}

fn generate_shadow_state(input: &StructParseInput) -> proc_macro2::TokenStream {
    let StructParseInput {
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

fn generate_shadow_patch_struct(input: &StructParseInput) -> proc_macro2::TokenStream {
    let StructParseInput {
        ident,
        generics,
        shadow_fields,
        copy_attrs,
        ..
    } = input;

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let optional_ident = format_ident!("Patch{}", ident);

    let assigners = create_assigners(&shadow_fields);
    let optional_fields = create_optional_fields(&shadow_fields);

    return quote! {
        #[automatically_derived]
        #[derive(Default, Clone, ::serde::Deserialize, ::serde::Serialize)]
        #(#copy_attrs)*
        pub struct #optional_ident #generics {
            #(
                #optional_fields
            ),*
        }

        #[automatically_derived]
        impl #impl_generics rustot::shadows::ShadowPatch for #ident #ty_generics #where_clause {
            type PatchState = #optional_ident;

            fn apply_patch(&mut self, opt: Self::PatchState) {
                #(
                    #assigners
                )*
            }
        }
    };
}

fn generate_shadow_patch_enum(input: &EnumParseInput) -> proc_macro2::TokenStream {
    let EnumParseInput {
        ident, generics, ..
    } = input;

    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    return quote! {
        #[automatically_derived]
        impl #impl_generics rustot::shadows::ShadowPatch for #ident #ty_generics #where_clause {
            type PatchState = #ident #ty_generics;

            fn apply_patch(&mut self, opt: Self::PatchState) {
                *self = opt;
            }
        }
    };
}
