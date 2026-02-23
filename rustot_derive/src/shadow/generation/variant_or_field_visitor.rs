use quote::quote;
use syn::{
    parse_quote, punctuated::Punctuated, spanned::Spanned, Attribute, DataStruct, Field, Fields,
    Ident, Token, Type, Visibility,
};

use crate::shadow::{DEFAULT_ATTRIBUTE, SHADOW_ATTRIBUTE};

pub trait VariantOrFieldVisitor {
    fn visit_field(&mut self, old: &mut syn::Field, new: &mut syn::Field);

    // Default to running the field visitor for each field in the variant
    fn visit_variant(&mut self, old: &mut syn::Variant, new: &mut syn::Variant) {
        for (old_field, new_field) in old.fields.iter_mut().zip(new.fields.iter_mut()) {
            self.visit_field(old_field, new_field);
        }
    }
}

pub struct SetNewVisibilityVisitor(pub bool);

impl VariantOrFieldVisitor for SetNewVisibilityVisitor {
    fn visit_field(&mut self, _old: &mut syn::Field, new: &mut syn::Field) {
        if self.0 {
            new.vis = Visibility::Public(syn::token::Pub(new.vis.span()));
        }
    }

    // Do nothing for enums, as field & variant visibility always follows the visibility of the enum itself
    fn visit_variant(&mut self, _old: &mut syn::Variant, _new: &mut syn::Variant) {}
}

pub struct SetNewTypeVisitor(pub Ident);

impl VariantOrFieldVisitor for SetNewTypeVisitor {
    fn visit_field(&mut self, old: &mut syn::Field, new: &mut syn::Field) {
        let newtype_ident = &self.0;

        let (is_primitive, inner_type, is_base_opt) = match extract_type_from_option(&old.ty) {
            Some(inner_ty) if is_primitive(&inner_ty) || has_shadow_arg(&old.attrs, "leaf") => {
                (true, &old.ty, true)
            }
            Some(inner_ty) => (false, inner_ty, true),
            None => (is_primitive(&old.ty), &old.ty, false),
        };

        let new_type = if is_primitive || has_shadow_arg(&old.attrs, "leaf") {
            quote! {Option<#inner_type>}
        } else if is_base_opt {
            quote! {Option<Option<<#inner_type as rustot::shadows::ShadowPatch>::#newtype_ident>>}
        } else {
            quote! {Option<<#inner_type as rustot::shadows::ShadowPatch>::#newtype_ident>}
        };

        new.ty = syn::parse2(new_type).unwrap();
    }
}

pub struct AddSerdeSkipAttribute;

impl VariantOrFieldVisitor for AddSerdeSkipAttribute {
    fn visit_field(&mut self, _old: &mut syn::Field, new: &mut syn::Field) {
        let attribute: Attribute =
            parse_quote! { #[serde(skip_serializing_if = "Option::is_none")] };
        new.attrs.push(attribute);
    }
}

pub struct RemoveShadowAttributesVisitor;

impl VariantOrFieldVisitor for RemoveShadowAttributesVisitor {
    fn visit_field(&mut self, old: &mut syn::Field, new: &mut syn::Field) {
        let indexes_to_remove = old
            .attrs
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                if a.path().is_ident(SHADOW_ATTRIBUTE) {
                    Some(i)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        // Don't forget to reverse so the indices are removed without being shifted!
        for i in indexes_to_remove.into_iter().rev() {
            new.attrs.swap_remove(i);
        }
    }

    fn visit_variant(&mut self, old: &mut syn::Variant, new: &mut syn::Variant) {
        let indexes_to_remove = old
            .attrs
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                if a.path().is_ident(DEFAULT_ATTRIBUTE) || a.path().is_ident(SHADOW_ATTRIBUTE) {
                    Some(i)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        // Don't forget to reverse so the indices are removed without being shifted!
        for i in indexes_to_remove.into_iter().rev() {
            new.attrs.swap_remove(i);
        }

        for (old_field, new_field) in old.fields.iter_mut().zip(new.fields.iter_mut()) {
            self.visit_field(old_field, new_field);
        }
    }
}

/// Rust primitive types: https://doc.rust-lang.org/reference/types.html
pub fn is_primitive(t: &Type) -> bool {
    match &t {
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|ps| {
                [
                    "bool", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
                    "i128", "isize", "f32", "f64", "char",
                ]
                .iter()
                .any(|s| ps.ident == *s)
            })
            .unwrap_or(false),
        Type::Paren(type_paren) => is_primitive(&type_paren.elem),
        Type::Reference(_) | Type::Array(_) | Type::Tuple(_) => false,
        t => panic!("Unsupported type: {:?}", quote! { #t }),
    }
}

pub fn borrow_fields(data_struct: &DataStruct) -> &Punctuated<Field, Token![,]> {
    match &data_struct.fields {
        Fields::Unnamed(f) => &f.unnamed,
        Fields::Named(f) => &f.named,
        Fields::Unit => unreachable!("Unit structs are not supported"),
    }
}

pub fn borrow_fields_mut(data_struct: &mut DataStruct) -> &mut Punctuated<Field, Token![,]> {
    match &mut data_struct.fields {
        Fields::Unnamed(f) => &mut f.unnamed,
        Fields::Named(f) => &mut f.named,
        Fields::Unit => unreachable!("Unit structs are not supported"),
    }
}

pub fn get_attr(attrs: &Vec<Attribute>, attr: &str) -> Option<Attribute> {
    attrs.iter().find(|a| a.path().is_ident(attr)).cloned()
}

pub fn has_shadow_arg(attrs: &Vec<Attribute>, arg: &str) -> bool {
    if let Some(a) = get_attr(&attrs, SHADOW_ATTRIBUTE) {
        let shadow_args = a
            .parse_args_with(Punctuated::<Ident, Token![,]>::parse_terminated)
            .unwrap_or_default();

        return shadow_args.iter().any(|i| i.to_string() == arg);
    }

    false
}

pub fn extract_type_from_option(ty: &syn::Type) -> Option<&syn::Type> {
    use syn::{GenericArgument, Path, PathArguments, PathSegment};

    fn extract_type_path(ty: &syn::Type) -> Option<&Path> {
        match *ty {
            syn::Type::Path(ref typepath) if typepath.qself.is_none() => Some(&typepath.path),
            _ => None,
        }
    }

    // TODO store (with lazy static) the vec of string
    // TODO maybe optimization, reverse the order of segments
    fn extract_option_segment(path: &Path) -> Option<&PathSegment> {
        let idents_of_path = path
            .segments
            .iter()
            .into_iter()
            .fold(String::new(), |mut acc, v| {
                acc.push_str(&v.ident.to_string());
                acc.push('|');
                acc
            });
        vec!["Option|", "std|option|Option|", "core|option|Option|"]
            .into_iter()
            .find(|s| &idents_of_path == *s)
            .and_then(|_| path.segments.last())
    }

    extract_type_path(ty)
        .and_then(|path| extract_option_segment(path))
        .and_then(|path_seg| {
            let type_params = &path_seg.arguments;
            // It should have only on angle-bracketed param ("<String>"):
            match *type_params {
                PathArguments::AngleBracketed(ref params) => params.args.first(),
                _ => None,
            }
        })
        .and_then(|generic_arg| match *generic_arg {
            GenericArgument::Type(ref ty) => Some(ty),
            _ => None,
        })
}
