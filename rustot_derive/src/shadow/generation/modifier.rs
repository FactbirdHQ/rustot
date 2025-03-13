use quote::format_ident;
use syn::{parse_quote, punctuated::Punctuated, Data, DeriveInput, Field, Ident, Path, Token};

use super::variant_or_field_visitor::{
    borrow_fields, borrow_fields_mut, extract_type_from_option, has_shadow_arg,
};

pub trait Modifier {
    fn modify(&mut self, original: &DeriveInput, output: &mut DeriveInput);
}

/// Rename the `output` struct to `self.0`, directly in the AST.
pub struct RenameModifier(pub String);

impl Modifier for RenameModifier {
    fn modify(&mut self, _original: &DeriveInput, output: &mut DeriveInput) {
        output.ident = Ident::new(&self.0, output.ident.span());
    }
}

pub struct WithDerivesModifier(pub &'static [&'static str]);

impl Modifier for WithDerivesModifier {
    fn modify(&mut self, _original: &DeriveInput, output: &mut DeriveInput) {
        let mut all_derives = self
            .0
            .iter()
            .filter(|s| {
                if matches!(output.data, Data::Enum(_)) {
                    return **s != "Default";
                }
                true
            })
            .map(|s| Path::from(format_ident!("{}", s)))
            .collect::<Punctuated<Path, Token![,]>>();

        output.attrs.retain(|attr| {
            if attr.path().is_ident("derive") {
                let derived = attr
                    .parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)
                    .unwrap_or_default();
                for derived_trait in derived.iter() {
                    if !all_derives.iter().any(|p| p == derived_trait) {
                        if !(matches!(output.data, Data::Enum(_))
                            && derived_trait.is_ident(&format_ident!("Default")))
                        {
                            all_derives.push(derived_trait.clone());
                        }
                    }
                }
                return false;
            }
            true
        });

        // Make sure we get derives first
        output.attrs.reverse();
        output.attrs.push(parse_quote! { #[derive(#all_derives)] });
        output.attrs.reverse();
    }
}

/// Filter all fields annotated with `#[shadow_attr(report_only)]` from the
/// `output` AST.
pub struct ReportOnlyModifier;

impl ReportOnlyModifier {
    fn filter_report_only(field: &Field) -> bool {
        if has_shadow_arg(field, "report_only") {
            return false;
        }

        if extract_type_from_option(&field.ty).is_some() {
            panic!("Optionals are only allowed in `report_only` fields");
        }

        true
    }
}

impl Modifier for ReportOnlyModifier {
    fn modify(&mut self, original: &DeriveInput, output: &mut DeriveInput) {
        // This modifier only modifies structs
        match (&original.data, &mut output.data) {
            (Data::Struct(data_struct_old), Data::Struct(data_struct_new)) => {
                let old_fields = borrow_fields(data_struct_old);
                let new_fields = borrow_fields_mut(data_struct_new);

                *new_fields = old_fields
                    .iter()
                    .zip(new_fields.iter_mut())
                    .filter_map(|(old_field, new_field)| {
                        Self::filter_report_only(old_field).then_some(new_field.clone())
                    })
                    .collect::<Punctuated<Field, Token![,]>>();
            }
            _ => {}
        }
    }
}
