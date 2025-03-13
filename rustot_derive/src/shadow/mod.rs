mod generation;
pub mod shadow_patch;

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{DeriveInput, Expr, ExprLit, Lit, LitInt, LitStr};

pub const SHADOW_ATTRIBUTE: &str = "shadow_attr";
pub const DEFAULT_ATTRIBUTE: &str = "default";
pub const CFG_ATTRIBUTE: &str = "cfg";

#[derive(Default)]
struct MacroParameters {
    shadow_name: Option<LitStr>,
    topic_prefix: Option<LitStr>,
    max_payload_size: Option<LitInt>,
}

impl Parse for MacroParameters {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut out = MacroParameters::default();

        while let Ok(optional) = input.parse::<syn::MetaNameValue>() {
            match (optional.path.get_ident(), optional.value) {
                (
                    Some(ident),
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(v), ..
                    }),
                ) if ident == "name" => {
                    out.shadow_name = Some(v);
                }
                (
                    Some(ident),
                    Expr::Lit(ExprLit {
                        lit: Lit::Str(v), ..
                    }),
                ) if ident == "topic_prefix" => {
                    out.topic_prefix = Some(v);
                }
                (
                    Some(ident),
                    Expr::Lit(ExprLit {
                        lit: Lit::Int(v), ..
                    }),
                ) if ident == "max_payload_size" => {
                    out.max_payload_size = Some(v);
                }
                _ => {}
            }

            if input.parse::<syn::token::Comma>().is_err() {
                break;
            }
        }

        Ok(out)
    }
}

pub fn shadow(attr: TokenStream, input: TokenStream) -> TokenStream {
    let shadow_patch = shadow_patch::shadow_patch(attr.clone(), input.clone());

    let derive_input = syn::parse2::<DeriveInput>(input).unwrap();
    let macro_params = syn::parse2::<MacroParameters>(attr).unwrap();

    let (impl_generics, ty_generics, where_clause) = derive_input.generics.split_for_impl();
    let ident = &derive_input.ident;

    let name = match macro_params.shadow_name {
        Some(name) => quote! { Some(#name) },
        None => quote! { None },
    };

    let topic_prefix = match macro_params.topic_prefix {
        Some(prefix) => quote! { #prefix },
        None => quote! { "$aws" },
    };

    let max_payload_size = macro_params
        .max_payload_size
        .map_or(quote! { 512 }, |m| quote! { #m });

    quote! {
       #[automatically_derived]
       impl #impl_generics ::rustot::shadows::ShadowState for #ident #ty_generics #where_clause {
           const NAME: Option<&'static str> = #name;
           const PREFIX: &'static str = #topic_prefix;
           const MAX_PAYLOAD_SIZE: usize = #max_payload_size;
       }

       #shadow_patch
    }
}
