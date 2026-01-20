use syn::{
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
    Attribute, Ident, Lit, Token,
};

/// The attribute name for shadow field annotations
pub const SHADOW_ATTR: &str = "shadow_attr";

/// The attribute name for default variant marker
pub const DEFAULT_ATTR: &str = "default";

/// The attribute name for cfg conditions
pub const CFG_ATTR: &str = "cfg";

/// The attribute name for serde attributes
pub const SERDE_ATTR: &str = "serde";

/// Parsed field-level shadow attributes
#[derive(Default, Clone)]
pub struct FieldAttrs {
    /// Field is a "leaf" - doesn't recursively implement ShadowPatch/ShadowNode
    pub leaf: bool,
    /// Field is only included in the Reported type, not Delta
    pub report_only: bool,
    /// Field is opaque - treated as leaf, requires MaxSize bound
    pub opaque: bool,
    /// Migration sources for this field
    pub migrate_from: Vec<String>,
    /// Conversion function for migration
    pub migrate_convert: Option<syn::Path>,
    /// Custom default value for this field
    pub default_value: Option<DefaultValue>,
}

/// Custom default value for a field
#[derive(Clone)]
pub enum DefaultValue {
    /// Literal value: `#[shadow_attr(default = 5000)]`
    Literal(Lit),
    /// Function path: `#[shadow_attr(default = my_default_fn)]`
    Function(syn::Path),
}

/// Single migration source specification
#[derive(Clone)]
struct MigrateSpec {
    from: Option<String>,
    convert: Option<syn::Path>,
}

impl FieldAttrs {
    /// Parse shadow attributes from a field's attribute list
    pub fn from_attrs(attrs: &[Attribute]) -> Self {
        let mut result = Self::default();

        for attr in attrs {
            if attr.path().is_ident(SHADOW_ATTR) {
                if let Ok(parsed) = attr.parse_args_with(ShadowAttrArgs::parse) {
                    for arg in parsed.args {
                        match arg {
                            ShadowAttrArg::Leaf => result.leaf = true,
                            ShadowAttrArg::ReportOnly => result.report_only = true,
                            ShadowAttrArg::Opaque => result.opaque = true,
                            ShadowAttrArg::Migrate(spec) => {
                                if let Some(from) = spec.from {
                                    result.migrate_from.push(from);
                                }
                                if spec.convert.is_some() {
                                    result.migrate_convert = spec.convert;
                                }
                            }
                            ShadowAttrArg::Default(val) => {
                                result.default_value = Some(val);
                            }
                        }
                    }
                }
            }
        }

        result
    }

    /// Check if this field is a leaf (primitive-like, no recursive patching)
    pub fn is_leaf(&self) -> bool {
        self.leaf || self.opaque
    }
}

/// Parsed arguments from #[shadow_attr(...)]
struct ShadowAttrArgs {
    args: Vec<ShadowAttrArg>,
}

impl Parse for ShadowAttrArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = Vec::new();

        while !input.is_empty() {
            args.push(input.parse()?);

            // Consume optional trailing comma
            let _ = input.parse::<Token![,]>();
        }

        Ok(Self { args })
    }
}

/// Single argument within #[shadow_attr(...)]
enum ShadowAttrArg {
    Leaf,
    ReportOnly,
    Opaque,
    Migrate(MigrateSpec),
    Default(DefaultValue),
}

impl Parse for ShadowAttrArg {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let ident: Ident = input.parse()?;

        match ident.to_string().as_str() {
            "leaf" => Ok(ShadowAttrArg::Leaf),
            "report_only" => Ok(ShadowAttrArg::ReportOnly),
            "opaque" => Ok(ShadowAttrArg::Opaque),
            "migrate" => {
                // Parse migrate(from = "...", convert = fn_name)
                let content;
                syn::parenthesized!(content in input);
                let spec = parse_migrate_spec(&content)?;
                Ok(ShadowAttrArg::Migrate(spec))
            }
            "default" => {
                // Parse default = value or default = fn_name
                input.parse::<Token![=]>()?;

                // Try parsing as literal first
                if let Ok(lit) = input.parse::<Lit>() {
                    Ok(ShadowAttrArg::Default(DefaultValue::Literal(lit)))
                } else {
                    // Parse as path (function reference)
                    let path: syn::Path = input.parse()?;
                    Ok(ShadowAttrArg::Default(DefaultValue::Function(path)))
                }
            }
            other => Err(syn::Error::new(
                ident.span(),
                format!("unknown shadow_attr argument: `{}`", other),
            )),
        }
    }
}

/// Parse the contents of migrate(...)
fn parse_migrate_spec(input: ParseStream) -> syn::Result<MigrateSpec> {
    let mut spec = MigrateSpec {
        from: None,
        convert: None,
    };

    let pairs: Punctuated<MigrateKeyValue, Token![,]> = Punctuated::parse_terminated(input)?;

    for pair in pairs {
        match pair.key.to_string().as_str() {
            "from" => {
                if let Lit::Str(s) = pair.value {
                    spec.from = Some(s.value());
                } else {
                    return Err(syn::Error::new_spanned(
                        pair.value,
                        "migrate(from = ...) expects a string literal",
                    ));
                }
            }
            "convert" => {
                if let Some(path) = pair.path_value {
                    spec.convert = Some(path);
                } else {
                    return Err(syn::Error::new(
                        pair.key.span(),
                        "migrate(convert = ...) expects a function path",
                    ));
                }
            }
            other => {
                return Err(syn::Error::new(
                    pair.key.span(),
                    format!("unknown migrate argument: `{}`", other),
                ));
            }
        }
    }

    Ok(spec)
}

/// Key-value pair in migrate(key = value, ...)
struct MigrateKeyValue {
    key: Ident,
    value: Lit,
    path_value: Option<syn::Path>,
}

impl Parse for MigrateKeyValue {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        input.parse::<Token![=]>()?;

        // Try parsing as literal first, then as path
        let (value, path_value) = if input.peek(syn::Lit) {
            (input.parse()?, None)
        } else {
            let path: syn::Path = input.parse()?;
            // Use a dummy literal since we have a path
            (Lit::Bool(syn::LitBool::new(false, key.span())), Some(path))
        };

        Ok(Self {
            key,
            value,
            path_value,
        })
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

/// Extract serde rename from field attributes
pub fn get_serde_rename(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident(SERDE_ATTR) {
            if let Ok(nested) =
                attr.parse_args_with(Punctuated::<syn::Meta, Token![,]>::parse_terminated)
            {
                for meta in nested {
                    if let syn::Meta::NameValue(nv) = meta {
                        if nv.path.is_ident("rename") {
                            if let syn::Expr::Lit(syn::ExprLit {
                                lit: Lit::Str(s), ..
                            }) = nv.value
                            {
                                return Some(s.value());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract serde rename_all from type/variant attributes
pub fn get_serde_rename_all(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident(SERDE_ATTR) {
            if let Ok(nested) =
                attr.parse_args_with(Punctuated::<syn::Meta, Token![,]>::parse_terminated)
            {
                for meta in nested {
                    if let syn::Meta::NameValue(nv) = meta {
                        if nv.path.is_ident("rename_all") {
                            if let syn::Expr::Lit(syn::ExprLit {
                                lit: Lit::Str(s), ..
                            }) = nv.value
                            {
                                return Some(s.value());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract serde tag and content for adjacently-tagged enums
pub fn get_serde_tag_content(attrs: &[Attribute]) -> (Option<String>, Option<String>) {
    let mut tag = None;
    let mut content = None;

    for attr in attrs {
        if attr.path().is_ident(SERDE_ATTR) {
            if let Ok(nested) =
                attr.parse_args_with(Punctuated::<syn::Meta, Token![,]>::parse_terminated)
            {
                for meta in nested {
                    if let syn::Meta::NameValue(nv) = meta {
                        if nv.path.is_ident("tag") {
                            if let syn::Expr::Lit(syn::ExprLit {
                                lit: Lit::Str(s), ..
                            }) = nv.value
                            {
                                tag = Some(s.value());
                            }
                        } else if nv.path.is_ident("content") {
                            if let syn::Expr::Lit(syn::ExprLit {
                                lit: Lit::Str(s), ..
                            }) = nv.value
                            {
                                content = Some(s.value());
                            }
                        }
                    }
                }
            }
        }
    }

    (tag, content)
}

/// Get the serde-renamed variant name, respecting explicit renames and rename_all
pub fn get_variant_serde_name(variant: &syn::Variant, rename_all: Option<&str>) -> String {
    get_serde_rename(&variant.attrs).unwrap_or_else(|| {
        if let Some(convention) = rename_all {
            apply_rename_all(&variant.ident.to_string(), convention)
        } else {
            variant.ident.to_string()
        }
    })
}

/// Apply rename_all convention to a name
pub fn apply_rename_all(name: &str, convention: &str) -> String {
    match convention {
        "lowercase" => name.to_lowercase(),
        "UPPERCASE" => name.to_uppercase(),
        "camelCase" => to_camel_case(name),
        "snake_case" => to_snake_case(name),
        "SCREAMING_SNAKE_CASE" => to_snake_case(name).to_uppercase(),
        "kebab-case" => to_snake_case(name).replace('_', "-"),
        "SCREAMING-KEBAB-CASE" => to_snake_case(name).to_uppercase().replace('_', "-"),
        "PascalCase" => to_pascal_case(name),
        _ => name.to_string(),
    }
}

fn to_camel_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = false;

    for (i, c) in s.chars().enumerate() {
        if c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else if i == 0 {
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

fn to_pascal_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
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

    #[test]
    fn test_parse_opaque() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(opaque)])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert!(field_attrs.opaque);
        assert!(field_attrs.is_leaf());
    }

    #[test]
    fn test_parse_migrate_from() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(migrate(from = "/old_key"))])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert_eq!(field_attrs.migrate_from, vec!["/old_key".to_string()]);
    }

    #[test]
    fn test_parse_migrate_with_convert() {
        let attrs: Vec<Attribute> =
            vec![parse_quote!(#[shadow_attr(migrate(from = "/old_key", convert = my_convert))])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert_eq!(field_attrs.migrate_from, vec!["/old_key".to_string()]);
        assert!(field_attrs.migrate_convert.is_some());
    }

    #[test]
    fn test_parse_default_literal() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(default = 5000)])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert!(field_attrs.default_value.is_some());
        match field_attrs.default_value {
            Some(DefaultValue::Literal(Lit::Int(i))) => {
                assert_eq!(i.base10_parse::<u32>().unwrap(), 5000);
            }
            _ => panic!("Expected literal int"),
        }
    }

    #[test]
    fn test_parse_default_bool() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(default = true)])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert!(field_attrs.default_value.is_some());
        match field_attrs.default_value {
            Some(DefaultValue::Literal(Lit::Bool(b))) => {
                assert!(b.value);
            }
            _ => panic!("Expected literal bool"),
        }
    }

    #[test]
    fn test_rename_all_lowercase() {
        assert_eq!(apply_rename_all("MyVariant", "lowercase"), "myvariant");
    }

    #[test]
    fn test_rename_all_snake_case() {
        assert_eq!(apply_rename_all("MyVariant", "snake_case"), "my_variant");
    }

    #[test]
    fn test_rename_all_camel_case() {
        assert_eq!(apply_rename_all("my_variant", "camelCase"), "myVariant");
    }
}
