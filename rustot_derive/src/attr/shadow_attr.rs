use syn::{
    parse::{Parse, ParseStream},
    Expr, ExprLit, Ident, Lit, LitBool, LitInt, LitStr, Path, Token,
};

/// Default AWS IoT topic prefix
pub const DEFAULT_TOPIC_PREFIX: &str = "$aws";

/// Default maximum payload size in bytes
pub const DEFAULT_MAX_PAYLOAD_SIZE: usize = 512;

/// Parameters for the #[shadow(...)] macro (legacy)
#[derive(Default)]
pub struct ShadowParams {
    /// Optional shadow name (for named shadows)
    pub name: Option<LitStr>,
    /// Topic prefix (defaults to "$aws")
    pub topic_prefix: Option<LitStr>,
    /// Maximum payload size (defaults to 512)
    pub max_payload_size: Option<LitInt>,
    /// Custom reported type (skips generating Reported struct)
    pub reported: Option<Path>,
}

impl Parse for ShadowParams {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut params = Self::default();

        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match ident.to_string().as_str() {
                "name" => {
                    params.name = Some(input.parse()?);
                }
                "topic_prefix" => {
                    params.topic_prefix = Some(input.parse()?);
                }
                "max_payload_size" => {
                    params.max_payload_size = Some(input.parse()?);
                }
                "reported" => {
                    params.reported = Some(input.parse()?);
                }
                unknown => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown shadow attribute: `{}`", unknown),
                    ));
                }
            }

            // Consume optional trailing comma
            let _ = input.parse::<Token![,]>();
        }

        Ok(params)
    }
}

/// Parameters for the #[shadow_patch(...)] macro (legacy)
#[derive(Debug)]
pub struct ShadowPatchParams {
    /// Whether to automatically derive common traits (default: true)
    pub auto_derive: bool,
    /// Whether to skip generating Default impl for enums (default: false)
    pub no_default: bool,
}

impl Default for ShadowPatchParams {
    fn default() -> Self {
        Self {
            auto_derive: true,
            no_default: false,
        }
    }
}

impl Parse for ShadowPatchParams {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut params = Self::default();

        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: Expr = input.parse()?;

            let bool_value = match &value {
                Expr::Lit(ExprLit {
                    lit: Lit::Bool(LitBool { value, .. }),
                    ..
                }) => *value,
                _ => {
                    return Err(syn::Error::new_spanned(
                        &value,
                        "expected boolean literal (true or false)",
                    ));
                }
            };

            match ident.to_string().as_str() {
                "auto_derive" => {
                    params.auto_derive = bool_value;
                }
                "no_default" => {
                    params.no_default = bool_value;
                }
                unknown => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown shadow_patch attribute: `{}`", unknown),
                    ));
                }
            }

            // Consume optional trailing comma
            let _ = input.parse::<Token![,]>();
        }

        Ok(params)
    }
}

// =============================================================================
// New KV-based shadow macros (Phase 8)
// =============================================================================

/// Parameters for the #[shadow_root(name = "...")] macro
///
/// This macro marks a struct as a top-level shadow with KV persistence support.
/// It implements both `ShadowRoot` and `ShadowNode` traits.
#[derive(Default)]
pub struct ShadowRootParams {
    /// Shadow name (required for named shadows, None for classic)
    pub name: Option<LitStr>,
}

impl Parse for ShadowRootParams {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut params = Self::default();

        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match ident.to_string().as_str() {
                "name" => {
                    params.name = Some(input.parse()?);
                }
                unknown => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown shadow_root attribute: `{}`", unknown),
                    ));
                }
            }

            // Consume optional trailing comma
            let _ = input.parse::<Token![,]>();
        }

        Ok(params)
    }
}

/// Parameters for the #[shadow_node] macro (no parameters currently)
///
/// This macro marks a struct or enum as a nested shadow type with KV persistence
/// support. It implements the `ShadowNode` trait.
#[derive(Default)]
pub struct ShadowNodeParams {}

impl Parse for ShadowNodeParams {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Currently no parameters, but parse for future extensibility
        if !input.is_empty() {
            let ident: Ident = input.parse()?;
            return Err(syn::Error::new(
                ident.span(),
                format!("unknown shadow_node attribute: `{}`", ident),
            ));
        }
        Ok(Self::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_shadow_params_empty() {
        let params: ShadowParams = syn::parse2(quote::quote!()).unwrap();
        assert!(params.name.is_none());
        assert!(params.topic_prefix.is_none());
        assert!(params.max_payload_size.is_none());
    }

    #[test]
    fn test_parse_shadow_params_full() {
        let params: ShadowParams = syn::parse2(quote::quote!(
            name = "test",
            topic_prefix = "$custom",
            max_payload_size = 1024
        ))
        .unwrap();
        assert_eq!(params.name.unwrap().value(), "test");
        assert_eq!(params.topic_prefix.unwrap().value(), "$custom");
        assert_eq!(
            params
                .max_payload_size
                .unwrap()
                .base10_parse::<usize>()
                .unwrap(),
            1024
        );
    }

    #[test]
    fn test_parse_shadow_patch_params_empty() {
        let params: ShadowPatchParams = syn::parse2(quote::quote!()).unwrap();
        assert!(params.auto_derive);
        assert!(!params.no_default);
    }

    #[test]
    fn test_parse_shadow_patch_params() {
        let params: ShadowPatchParams =
            syn::parse2(quote::quote!(auto_derive = false, no_default = true)).unwrap();
        assert!(!params.auto_derive);
        assert!(params.no_default);
    }

    #[test]
    fn test_unknown_attribute_error() {
        let result: syn::Result<ShadowParams> = syn::parse2(quote::quote!(unknown_attr = "value"));
        match result {
            Err(err) => assert!(err.to_string().contains("unknown shadow attribute")),
            Ok(_) => panic!("Expected error for unknown attribute"),
        }
    }

    #[test]
    fn test_parse_shadow_root_params_with_name() {
        let params: ShadowRootParams = syn::parse2(quote::quote!(name = "device")).unwrap();
        assert_eq!(params.name.unwrap().value(), "device");
    }

    #[test]
    fn test_parse_shadow_root_params_empty() {
        let params: ShadowRootParams = syn::parse2(quote::quote!()).unwrap();
        assert!(params.name.is_none());
    }

    #[test]
    fn test_parse_shadow_node_params_empty() {
        let params: ShadowNodeParams = syn::parse2(quote::quote!()).unwrap();
        // No assertions - just checking it parses
        let _ = params;
    }

    #[test]
    fn test_shadow_node_unknown_attr_error() {
        let result: syn::Result<ShadowNodeParams> =
            syn::parse2(quote::quote!(unknown_attr = "value"));
        match result {
            Err(err) => assert!(err.to_string().contains("unknown shadow_node attribute")),
            Ok(_) => panic!("Expected error for unknown attribute"),
        }
    }
}
