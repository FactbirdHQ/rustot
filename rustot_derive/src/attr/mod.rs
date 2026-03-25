mod field_attr;
mod shadow_attr;

#[cfg(feature = "kv_persist")]
pub use field_attr::DefaultValue;
pub use field_attr::{
    apply_rename_all, get_serde_rename, get_serde_rename_all, get_serde_tag_content,
    get_variant_serde_name, has_default_attr, FieldAttrs,
};
#[cfg(feature = "multi")]
pub use shadow_attr::MultiShadowRootParams;
pub use shadow_attr::{ShadowNodeParams, ShadowRootParams};
