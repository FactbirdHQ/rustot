mod shadow_patch_impl;
mod shadow_state_impl;
mod type_def;

pub use shadow_patch_impl::generate_shadow_patch_impl;
pub use shadow_state_impl::generate_shadow_state_impl;
pub use type_def::{generate_enum_default_impl, generate_shadow_patch_types, TypeDefConfig};
