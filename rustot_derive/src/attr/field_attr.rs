use syn::{punctuated::Punctuated, Attribute, Ident, Token};

/// The attribute name for shadow field annotations
pub const SHADOW_ATTR: &str = "shadow_attr";

/// The attribute name for default variant marker
pub const DEFAULT_ATTR: &str = "default";

/// The attribute name for cfg conditions
pub const CFG_ATTR: &str = "cfg";

/// Parsed field-level shadow attributes
#[derive(Debug, Default, Clone)]
pub struct FieldAttrs {
    /// Field is a "leaf" - doesn't recursively implement ShadowPatch
    pub leaf: bool,
    /// Field is only included in the Reported type, not Delta
    pub report_only: bool,
}

impl FieldAttrs {
    /// Parse shadow attributes from a field's attribute list
    pub fn from_attrs(attrs: &[Attribute]) -> Self {
        let mut result = Self::default();

        for attr in attrs {
            if attr.path().is_ident(SHADOW_ATTR) {
                if let Ok(args) =
                    attr.parse_args_with(Punctuated::<Ident, Token![,]>::parse_terminated)
                {
                    for arg in args {
                        match arg.to_string().as_str() {
                            "leaf" => result.leaf = true,
                            "report_only" => result.report_only = true,
                            _ => {} // Unknown arguments are silently ignored for now
                        }
                    }
                }
            }
        }

        result
    }

}

/// Get a specific attribute by name from an attribute list
pub fn get_attr<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a Attribute> {
    attrs.iter().find(|a| a.path().is_ident(name))
}

/// Check if a variant has the #[default] attribute
pub fn has_default_attr(attrs: &[Attribute]) -> bool {
    get_attr(attrs, DEFAULT_ATTR).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_parse_leaf() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(leaf)])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert!(field_attrs.leaf);
        assert!(!field_attrs.report_only);
    }

    #[test]
    fn test_parse_report_only() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(report_only)])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert!(!field_attrs.leaf);
        assert!(field_attrs.report_only);
    }

    #[test]
    fn test_parse_multiple() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(leaf, report_only)])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert!(field_attrs.leaf);
        assert!(field_attrs.report_only);
    }

}
