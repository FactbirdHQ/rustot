use quote::quote;
use syn::Type;

/// Rust primitive types that don't implement ShadowPatch
const PRIMITIVE_TYPES: &[&str] = &[
    "bool", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
    "f32", "f64", "char",
];

/// Check if a type is a Rust primitive type.
///
/// Primitive types are treated as "leaf" nodes in the shadow state tree,
/// meaning they don't recursively implement ShadowPatch.
pub fn is_primitive(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .map(|seg| PRIMITIVE_TYPES.iter().any(|&p| seg.ident == p))
            .unwrap_or(false),
        Type::Paren(type_paren) => is_primitive(&type_paren.elem),
        Type::Reference(_) | Type::Array(_) | Type::Tuple(_) => false,
        t => panic!("Unsupported type: {:?}", quote! { #t }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_primitives() {
        let ty: Type = parse_quote!(u32);
        assert!(is_primitive(&ty));

        let ty: Type = parse_quote!(bool);
        assert!(is_primitive(&ty));

        let ty: Type = parse_quote!(f64);
        assert!(is_primitive(&ty));
    }

    #[test]
    fn test_non_primitives() {
        let ty: Type = parse_quote!(String);
        assert!(!is_primitive(&ty));

        let ty: Type = parse_quote!(Vec<u8>);
        assert!(!is_primitive(&ty));

        let ty: Type = parse_quote!(MyStruct);
        assert!(!is_primitive(&ty));
    }

    #[test]
    fn test_parenthesized() {
        let ty: Type = parse_quote!((u32));
        assert!(is_primitive(&ty));
    }
}
