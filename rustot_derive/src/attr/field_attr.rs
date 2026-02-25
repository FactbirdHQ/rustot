use darling::FromMeta;
use heck::{
    ToKebabCase, ToLowerCamelCase, ToPascalCase, ToShoutyKebabCase, ToShoutySnakeCase, ToSnakeCase,
};
use syn::{punctuated::Punctuated, Attribute, Lit, Meta, Token};

/// The attribute name for shadow field annotations
pub const SHADOW_ATTR: &str = "shadow_attr";

/// The attribute name for default variant marker
pub const DEFAULT_ATTR: &str = "default";

/// The attribute name for serde attributes
pub const SERDE_ATTR: &str = "serde";

/// Parsed field-level shadow attributes
#[derive(Default, Clone, FromMeta)]
pub struct FieldAttrs {
    /// Field is only included in the Reported type, not Delta
    #[darling(default)]
    pub report_only: bool,
    /// Field is opaque - treated as leaf (primitive-like, no recursive patching)
    /// Can be just `opaque` (requires MaxSize) or `opaque(max_size = N)` (explicit size)
    #[darling(default)]
    pub opaque: Option<OpaqueSpec>,
    /// Migration specifications for this field
    #[cfg(feature = "kv_persist")]
    #[darling(default, multiple)]
    migrate: Vec<MigrateSpec>,
    /// Custom default value for this field
    #[cfg(feature = "kv_persist")]
    #[darling(default, rename = "default")]
    pub default_value: Option<DefaultValue>,
}

/// Opaque field specification
///
/// Supports two forms:
/// - `#[shadow_attr(opaque)]` - field type must implement `MaxSize`
/// - `#[shadow_attr(opaque(max_size = 64))]` - explicit max serialized size
#[derive(Clone, Default)]
pub struct OpaqueSpec {
    /// Explicit maximum serialized size in bytes (only used with kv_persist)
    #[cfg_attr(not(feature = "kv_persist"), allow(dead_code))]
    pub max_size: Option<usize>,
}

impl FromMeta for OpaqueSpec {
    fn from_word() -> darling::Result<Self> {
        // `#[shadow_attr(opaque)]` - no max_size specified
        Ok(OpaqueSpec { max_size: None })
    }

    fn from_list(items: &[darling::ast::NestedMeta]) -> darling::Result<Self> {
        // `#[shadow_attr(opaque(max_size = N))]`
        let mut max_size = None;
        for item in items {
            if let darling::ast::NestedMeta::Meta(Meta::NameValue(nv)) = item {
                if nv.path.is_ident("max_size") {
                    if let syn::Expr::Lit(syn::ExprLit {
                        lit: Lit::Int(lit_int),
                        ..
                    }) = &nv.value
                    {
                        max_size = Some(lit_int.base10_parse()?);
                    }
                }
            }
        }
        Ok(OpaqueSpec { max_size })
    }
}

impl FieldAttrs {
    /// Parse shadow attributes from a field's attribute list
    pub fn from_attrs(attrs: &[Attribute]) -> Self {
        for attr in attrs {
            if attr.path().is_ident(SHADOW_ATTR) {
                if let Meta::List(meta_list) = &attr.meta {
                    if let Ok(parsed) = Self::from_list(
                        &darling::ast::NestedMeta::parse_meta_list(meta_list.tokens.clone())
                            .unwrap_or_default(),
                    ) {
                        return parsed;
                    }
                }
            }
        }
        Self::default()
    }

    /// Check if this field is marked as opaque
    pub fn is_opaque(&self) -> bool {
        self.opaque.is_some()
    }

    /// Get the explicit max_size if specified, None if opaque without max_size or not opaque
    #[cfg_attr(not(feature = "kv_persist"), allow(dead_code))]
    pub fn opaque_max_size(&self) -> Option<usize> {
        self.opaque.as_ref().and_then(|o| o.max_size)
    }

    /// Get all migration source keys
    #[cfg(feature = "kv_persist")]
    pub fn migrate_from(&self) -> Vec<String> {
        self.migrate.iter().filter_map(|m| m.from.clone()).collect()
    }

    /// Get all migration source keys (always empty when kv_persist is disabled)
    #[cfg(not(feature = "kv_persist"))]
    pub fn migrate_from(&self) -> Vec<String> {
        Vec::new()
    }

    /// Get the migration conversion function (last one wins if multiple specified)
    #[cfg(feature = "kv_persist")]
    pub fn migrate_convert(&self) -> Option<syn::Path> {
        self.migrate
            .iter()
            .filter_map(|m| m.convert.clone())
            .next_back()
    }
}

/// Single migration source specification
#[cfg(feature = "kv_persist")]
#[derive(Clone, FromMeta)]
struct MigrateSpec {
    #[darling(default)]
    from: Option<String>,
    #[darling(default)]
    convert: Option<syn::Path>,
}

/// Custom default value for a field
#[cfg(feature = "kv_persist")]
#[derive(Clone)]
pub enum DefaultValue {
    /// Literal value: `#[shadow_attr(default = 5000)]`
    Literal(Lit),
    /// Function path: `#[shadow_attr(default = my_default_fn)]`
    Function(syn::Path),
}

#[cfg(feature = "kv_persist")]
impl FromMeta for DefaultValue {
    fn from_meta(item: &syn::Meta) -> darling::Result<Self> {
        match item {
            syn::Meta::NameValue(nv) => match &nv.value {
                syn::Expr::Lit(expr_lit) => Ok(DefaultValue::Literal(expr_lit.lit.clone())),
                syn::Expr::Path(expr_path) => Ok(DefaultValue::Function(expr_path.path.clone())),
                _ => Err(darling::Error::custom(
                    "expected literal or path for default value",
                )),
            },
            _ => Err(darling::Error::custom("expected name = value")),
        }
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

/// Extract a string value from a serde attribute by key name.
fn get_serde_str_value(attrs: &[Attribute], key: &str) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident(SERDE_ATTR) {
            if let Ok(nested) =
                attr.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            {
                for meta in nested {
                    if let Meta::NameValue(nv) = meta {
                        if nv.path.is_ident(key) {
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

/// Extract serde rename from field attributes
pub fn get_serde_rename(attrs: &[Attribute]) -> Option<String> {
    get_serde_str_value(attrs, "rename")
}

/// Extract serde rename_all from type/variant attributes
pub fn get_serde_rename_all(attrs: &[Attribute]) -> Option<String> {
    get_serde_str_value(attrs, "rename_all")
}

/// Extract serde tag and content for adjacently-tagged enums
pub fn get_serde_tag_content(attrs: &[Attribute]) -> (Option<String>, Option<String>) {
    (
        get_serde_str_value(attrs, "tag"),
        get_serde_str_value(attrs, "content"),
    )
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
        "camelCase" => name.to_lower_camel_case(),
        "snake_case" => name.to_snake_case(),
        "SCREAMING_SNAKE_CASE" => name.to_shouty_snake_case(),
        "kebab-case" => name.to_kebab_case(),
        "SCREAMING-KEBAB-CASE" => name.to_shouty_kebab_case(),
        "PascalCase" => name.to_pascal_case(),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    // Only test custom FromMeta impl for DefaultValue - Darling handles the rest
    #[cfg(feature = "kv_persist")]
    #[test]
    fn test_default_value_literal() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(default = 5000)])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        match field_attrs.default_value {
            Some(DefaultValue::Literal(Lit::Int(i))) => {
                assert_eq!(i.base10_parse::<u32>().unwrap(), 5000);
            }
            _ => panic!("Expected literal int"),
        }
    }

    #[cfg(feature = "kv_persist")]
    #[test]
    fn test_default_value_path() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(default = my_default_fn)])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        match field_attrs.default_value {
            Some(DefaultValue::Function(path)) => {
                assert!(path.is_ident("my_default_fn"));
            }
            _ => panic!("Expected function path"),
        }
    }

    // Test migrate accessor methods
    #[cfg(feature = "kv_persist")]
    #[test]
    fn test_migrate_from_accessor() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(migrate(from = "/old_key"))])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert_eq!(field_attrs.migrate_from(), vec!["/old_key".to_string()]);
    }

    #[cfg(feature = "kv_persist")]
    #[test]
    fn test_migrate_convert_accessor() {
        let attrs: Vec<Attribute> =
            vec![parse_quote!(#[shadow_attr(migrate(from = "/old", convert = my_convert))])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert!(field_attrs.migrate_convert().is_some());
    }

    // Test opaque attribute parsing
    #[test]
    fn test_opaque_simple() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(opaque)])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert!(field_attrs.is_opaque());
        assert_eq!(field_attrs.opaque_max_size(), None);
    }

    #[test]
    fn test_opaque_with_max_size() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(opaque(max_size = 64))])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert!(field_attrs.is_opaque());
        assert_eq!(field_attrs.opaque_max_size(), Some(64));
    }

    #[test]
    fn test_not_opaque() {
        let attrs: Vec<Attribute> = vec![parse_quote!(#[shadow_attr(report_only)])];
        let field_attrs = FieldAttrs::from_attrs(&attrs);
        assert!(!field_attrs.is_opaque());
        assert_eq!(field_attrs.opaque_max_size(), None);
    }

    // Test heck integration for rename_all
    #[test]
    fn test_rename_all_snake_case() {
        assert_eq!(apply_rename_all("MyVariant", "snake_case"), "my_variant");
    }

    #[test]
    fn test_rename_all_camel_case() {
        assert_eq!(apply_rename_all("my_variant", "camelCase"), "myVariant");
    }
}
