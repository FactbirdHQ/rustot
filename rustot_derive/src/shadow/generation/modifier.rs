use quote::format_ident;
use syn::{parse_quote, punctuated::Punctuated, Data, DeriveInput, Field, Ident, Path, Token};

use super::variant_or_field_visitor::{
    borrow_fields, borrow_fields_mut, extract_type_from_option, has_shadow_arg,
    is_report_only_persist, is_report_only_stripped,
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

pub struct WithDerivesModifier(pub bool, pub Vec<&'static str>);

impl Modifier for WithDerivesModifier {
    fn modify(&mut self, _original: &DeriveInput, output: &mut DeriveInput) {
        if !self.0 {
            return;
        }

        let mut all_derives = self
            .1
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

/// Filter fields annotated with `#[shadow_attr(report_only)]` from the
/// `output` AST.
///
/// - `strip_persist: false` — only strip plain `report_only` fields (keep
///   `report_only(persist)` in the struct). Used for the original struct
///   and the Reported `From` impl.
/// - `strip_persist: true` — strip ALL `report_only` variants including
///   `report_only(persist)`. Used for the Delta struct.
pub struct ReportOnlyModifier {
    pub strip_persist: bool,
}

impl ReportOnlyModifier {
    fn should_keep(&self, field: &Field) -> bool {
        let is_any_report_only = has_shadow_arg(&field.attrs, "report_only");

        if is_any_report_only {
            if self.strip_persist {
                // Delta mode: strip all report_only variants
                return false;
            }
            // Struct mode: only strip plain report_only, keep persist
            if is_report_only_stripped(&field.attrs) {
                return false;
            }
            // report_only(persist): keep in struct, but validate no bare Option
            if is_report_only_persist(&field.attrs) && extract_type_from_option(&field.ty).is_some()
            {
                panic!("Optionals are only allowed in plain `report_only` fields, not `report_only(persist)`");
            }
            return true;
        }

        // Normal field: Option types are not allowed
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
                        self.should_keep(old_field).then_some(new_field.clone())
                    })
                    .collect::<Punctuated<Field, Token![,]>>();
            }
            _ => {}
        }
    }
}
