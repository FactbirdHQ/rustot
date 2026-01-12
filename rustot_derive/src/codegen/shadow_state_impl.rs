use proc_macro2::TokenStream;
use quote::quote;
use syn::DeriveInput;

use crate::attr::{ShadowParams, DEFAULT_MAX_PAYLOAD_SIZE, DEFAULT_TOPIC_PREFIX};

/// Generate the ShadowState trait implementation
pub fn generate_shadow_state_impl(input: &DeriveInput, params: &ShadowParams) -> TokenStream {
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let ident = &input.ident;

    let name = match &params.name {
        Some(name) => quote! { Some(#name) },
        None => quote! { None },
    };

    let topic_prefix = match &params.topic_prefix {
        Some(prefix) => quote! { #prefix },
        None => {
            let default = DEFAULT_TOPIC_PREFIX;
            quote! { #default }
        }
    };

    let max_payload_size = params
        .max_payload_size
        .as_ref()
        .map(|m| quote! { #m })
        .unwrap_or_else(|| {
            let default = DEFAULT_MAX_PAYLOAD_SIZE;
            quote! { #default }
        });

    quote! {
        #[automatically_derived]
        impl #impl_generics rustot::shadows::ShadowState for #ident #ty_generics #where_clause {
            const NAME: Option<&'static str> = #name;
            const PREFIX: &'static str = #topic_prefix;
            const MAX_PAYLOAD_SIZE: usize = #max_payload_size;
        }
    }
}
