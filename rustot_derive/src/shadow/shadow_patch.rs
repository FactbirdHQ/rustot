use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::DeriveInput;

use crate::shadow::generation::{
    generator::{DefaultGenerator, GenerateFromImpl, NewGenerator},
    modifier::{RenameModifier, ReportOnlyModifier, WithDerivesModifier},
    variant_or_field_visitor::{
        AddSerdeSkipAttribute, RemoveShadowAttributesVisitor, SetNewTypeVisitor,
        SetNewVisibilityVisitor,
    },
    GenerateShadowPatchImplVisitor, ShadowGenerator,
};

pub fn shadow_patch(_attr: TokenStream, input: TokenStream) -> TokenStream {
    let full_input = syn::parse2::<DeriveInput>(input).unwrap();

    let original_ident = full_input.ident.to_string();

    let delta = format_ident!("Delta");
    let reported = format_ident!("Reported");
    let reported_ident = format_ident!("Reported{}", &original_ident);

    let desired_tokens = {
        ShadowGenerator::new(full_input.clone())
            .modifier(&mut ReportOnlyModifier)
            .modifier(&mut RenameModifier(original_ident.clone()))
            .modifier(&mut WithDerivesModifier(&[
                "Serialize",
                "Deserialize",
                "Default",
                "Clone",
            ]))
            .generator(&mut DefaultGenerator)
            .variant_or_field_visitor(&mut RemoveShadowAttributesVisitor)
            .generator(&mut NewGenerator)
            .finalize()
    };

    let delta_tokens = {
        let mut shadowpatch_impl_generator =
            GenerateShadowPatchImplVisitor::new(reported_ident.clone());

        ShadowGenerator::new(full_input.clone())
            .modifier(&mut ReportOnlyModifier)
            .variant_or_field_visitor(&mut SetNewVisibilityVisitor(true))
            .variant_or_field_visitor(&mut SetNewTypeVisitor(delta.clone()))
            .variant_or_field_visitor(&mut shadowpatch_impl_generator)
            .modifier(&mut RenameModifier(format!(
                "{}{}",
                &delta, &original_ident
            )))
            .modifier(&mut WithDerivesModifier(&["Deserialize", "Clone"]))
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
            .modifier(&mut WithDerivesModifier(&[
                "Serialize",
                "Deserialize",
                "Default",
            ]))
            .generator(&mut DefaultGenerator)
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
