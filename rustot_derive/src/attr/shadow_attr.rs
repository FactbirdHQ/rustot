use darling::FromMeta;

/// Parameters for the #[shadow_root(name = "...")] macro
///
/// This macro marks a struct as a top-level shadow with KV persistence support.
/// It implements both `ShadowRoot` and `ShadowNode` traits.
#[derive(Default, FromMeta)]
pub struct ShadowRootParams {
    /// Shadow name (required for named shadows, None for classic)
    #[darling(default)]
    pub name: Option<String>,
    /// Topic prefix for MQTT topics (e.g., "$aws" for AWS IoT)
    #[darling(default)]
    pub topic_prefix: Option<String>,
    /// Maximum payload size for shadow documents
    #[darling(default)]
    pub max_payload_len: Option<usize>,
}

/// Parameters for the #[shadow_node] macro (no parameters currently)
///
/// This macro marks a struct or enum as a nested shadow type with KV persistence
/// support. It implements the `ShadowNode` trait.
#[derive(Default, FromMeta)]
pub struct ShadowNodeParams {}
