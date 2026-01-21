use syn::{
    parse::{Parse, ParseStream},
    Ident, LitInt, LitStr, Token,
};

// =============================================================================
// KV-based shadow macros (Phase 8)
// =============================================================================

/// Parameters for the #[shadow_root(name = "...")] macro
///
/// This macro marks a struct as a top-level shadow with KV persistence support.
/// It implements both `ShadowRoot` and `ShadowNode` traits.
#[derive(Default)]
pub struct ShadowRootParams {
    /// Shadow name (required for named shadows, None for classic)
    pub name: Option<LitStr>,
    /// Topic prefix for MQTT topics (e.g., "$aws" for AWS IoT)
    pub topic_prefix: Option<LitStr>,
    /// Maximum payload size for shadow documents
    pub max_payload_len: Option<LitInt>,
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
                "topic_prefix" => {
                    params.topic_prefix = Some(input.parse()?);
                }
                "max_payload_len" => {
                    params.max_payload_len = Some(input.parse()?);
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
    fn test_parse_shadow_root_params_with_name() {
        let params: ShadowRootParams = syn::parse2(quote::quote!(name = "device")).unwrap();
        assert_eq!(params.name.unwrap().value(), "device");
    }

    #[test]
    fn test_parse_shadow_root_params_empty() {
        let params: ShadowRootParams = syn::parse2(quote::quote!()).unwrap();
        assert!(params.name.is_none());
        assert!(params.topic_prefix.is_none());
        assert!(params.max_payload_len.is_none());
    }

    #[test]
    fn test_parse_shadow_root_params_with_topic_prefix() {
        let params: ShadowRootParams =
            syn::parse2(quote::quote!(name = "device", topic_prefix = "$aws")).unwrap();
        assert_eq!(params.name.unwrap().value(), "device");
        assert_eq!(params.topic_prefix.unwrap().value(), "$aws");
        assert!(params.max_payload_len.is_none());
    }

    #[test]
    fn test_parse_shadow_root_params_with_max_payload_len() {
        let params: ShadowRootParams =
            syn::parse2(quote::quote!(name = "device", max_payload_len = 1024)).unwrap();
        assert_eq!(params.name.unwrap().value(), "device");
        assert!(params.topic_prefix.is_none());
        assert_eq!(
            params.max_payload_len.unwrap().base10_parse::<usize>().unwrap(),
            1024
        );
    }

    #[test]
    fn test_parse_shadow_root_params_all() {
        let params: ShadowRootParams = syn::parse2(quote::quote!(
            name = "device",
            topic_prefix = "$custom",
            max_payload_len = 2048
        ))
        .unwrap();
        assert_eq!(params.name.unwrap().value(), "device");
        assert_eq!(params.topic_prefix.unwrap().value(), "$custom");
        assert_eq!(
            params.max_payload_len.unwrap().base10_parse::<usize>().unwrap(),
            2048
        );
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
