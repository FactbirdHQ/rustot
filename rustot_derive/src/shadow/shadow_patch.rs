use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    DeriveInput, Path,
};

use crate::shadow::generation::{
    generator::{DefaultGenerator, GenerateFromImpl, NewGenerator},
    modifier::{RenameModifier, ReportOnlyModifier, WithDerivesModifier},
    variant_or_field_visitor::{
        AddSerdeSkipAttribute, RemoveShadowAttributesVisitor, SetNewTypeVisitor,
        SetNewVisibilityVisitor,
    },
    GenerateShadowPatchImplVisitor, ShadowGenerator,
};

#[derive(Default)]
struct MacroParameters {
    auto_derive: Option<bool>,
    no_default: Option<bool>,
}

impl Parse for MacroParameters {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut out = MacroParameters::default();

        while let Ok(optional) = input.parse::<syn::MetaNameValue>() {
            match (optional.path.get_ident(), optional.value) {
                (
                    Some(ident),
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Bool(v),
                        ..
                    }),
                ) if ident == "auto_derive" => {
                    out.auto_derive = Some(v.value);
                }
                (
                    Some(ident),
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Bool(v),
                        ..
                    }),
                ) if ident == "no_default" => {
                    out.no_default = Some(v.value);
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

pub fn shadow_patch(attr: TokenStream, input: TokenStream) -> TokenStream {
    let full_input = syn::parse2::<DeriveInput>(input).unwrap();
    let macro_params = syn::parse2::<MacroParameters>(attr).unwrap();

    let original_ident = full_input.ident.to_string();

    let delta = format_ident!("Delta");
    let reported = format_ident!("Reported");
    let reported_ident = format_ident!("Reported{}", &original_ident);

    let desired_tokens = {
        let mut auto_derives = vec!["Serialize", "Deserialize", "Clone"];
        if !macro_params.no_default.unwrap_or_default() {
            auto_derives.push("Default");
        }

        ShadowGenerator::new(full_input.clone())
            .modifier(&mut ReportOnlyModifier)
            .modifier(&mut RenameModifier(original_ident.clone()))
            .modifier(&mut WithDerivesModifier(
                macro_params.auto_derive.unwrap_or(true),
                auto_derives,
            ))
            .generator(&mut DefaultGenerator(
                macro_params.no_default.unwrap_or_default(),
            ))
            .variant_or_field_visitor(&mut RemoveShadowAttributesVisitor)
            .generator(&mut NewGenerator)
            .finalize()
    };

    let delta_tokens = {
        let mut shadowpatch_impl_generator =
            GenerateShadowPatchImplVisitor::new(Path::from(reported_ident.clone()));

        ShadowGenerator::new(full_input.clone())
            .modifier(&mut ReportOnlyModifier)
            .variant_or_field_visitor(&mut SetNewVisibilityVisitor(true))
            .variant_or_field_visitor(&mut SetNewTypeVisitor(delta.clone()))
            .variant_or_field_visitor(&mut shadowpatch_impl_generator)
            .modifier(&mut RenameModifier(format!(
                "{}{}",
                &delta, &original_ident
            )))
            .modifier(&mut WithDerivesModifier(
                macro_params.auto_derive.unwrap_or(true),
                vec!["Deserialize", "Clone"],
            ))
            .generator(&mut shadowpatch_impl_generator)
            .variant_or_field_visitor(&mut RemoveShadowAttributesVisitor)
            .generator(&mut NewGenerator)
            .finalize()
    };

    let reported_tokens = {
        ShadowGenerator::new(full_input)
            .variant_or_field_visitor(&mut SetNewVisibilityVisitor(true))
            .variant_or_field_visitor(&mut SetNewTypeVisitor(reported))
            .variant_or_field_visitor(&mut AddSerdeSkipAttribute)
            .modifier(&mut RenameModifier(reported_ident.to_string()))
            .modifier(&mut WithDerivesModifier(
                macro_params.auto_derive.unwrap_or(true),
                vec!["Serialize", "Default"],
            ))
            .generator(&mut DefaultGenerator(false))
            .variant_or_field_visitor(&mut RemoveShadowAttributesVisitor)
            .generator(&mut NewGenerator)
            .modifier(&mut ReportOnlyModifier)
            .generator(&mut GenerateFromImpl)
            .finalize()
    };

    quote! {
        #desired_tokens

        #delta_tokens

        #reported_tokens
    }
}
