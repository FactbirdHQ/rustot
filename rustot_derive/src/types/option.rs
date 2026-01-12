use syn::{GenericArgument, PathArguments, Type};

/// Extract the inner type from an Option<T> type.
///
/// Returns `Some(&inner_type)` if the type is `Option<T>`, `std::option::Option<T>`,
/// or `core::option::Option<T>`. Returns `None` otherwise.
pub fn extract_inner_from_option(ty: &Type) -> Option<&Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };

    // Check if qualified path (e.g., std::option::Option)
    if type_path.qself.is_some() {
        return None;
    }

    let last_segment = type_path.path.segments.last()?;

    // The final segment must be "Option"
    if last_segment.ident != "Option" {
        return None;
    }

    // Verify the path is actually Option (not SomeOtherModule::Option)
    // Accept: Option, std::option::Option, core::option::Option
    let segment_count = type_path.path.segments.len();
    if segment_count > 1 {
        let is_std_option = segment_count == 3
            && type_path.path.segments[0].ident == "std"
            && type_path.path.segments[1].ident == "option";

        let is_core_option = segment_count == 3
            && type_path.path.segments[0].ident == "core"
            && type_path.path.segments[1].ident == "option";

        if !is_std_option && !is_core_option {
            return None;
        }
    }

    // Extract the type parameter
    let PathArguments::AngleBracketed(args) = &last_segment.arguments else {
        return None;
    };

    args.args.first().and_then(|arg| {
        if let GenericArgument::Type(inner) = arg {
            Some(inner)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_simple_option() {
        let ty: Type = parse_quote!(Option<u32>);
        let inner = extract_inner_from_option(&ty);
        assert!(inner.is_some());
    }

    #[test]
    fn test_std_option() {
        let ty: Type = parse_quote!(std::option::Option<String>);
        let inner = extract_inner_from_option(&ty);
        assert!(inner.is_some());
    }

    #[test]
    fn test_core_option() {
        let ty: Type = parse_quote!(core::option::Option<String>);
        let inner = extract_inner_from_option(&ty);
        assert!(inner.is_some());
    }

    #[test]
    fn test_not_option() {
        let ty: Type = parse_quote!(Vec<u32>);
        assert!(extract_inner_from_option(&ty).is_none());

        let ty: Type = parse_quote!(u32);
        assert!(extract_inner_from_option(&ty).is_none());

        // Custom Option type from another module
        let ty: Type = parse_quote!(my_module::Option<u32>);
        assert!(extract_inner_from_option(&ty).is_none());
    }

    #[test]
    fn test_nested_option() {
        let ty: Type = parse_quote!(Option<Option<u32>>);
        let inner = extract_inner_from_option(&ty).unwrap();

        // The inner type should also be an Option
        assert!(extract_inner_from_option(inner).is_some());
    }
}
