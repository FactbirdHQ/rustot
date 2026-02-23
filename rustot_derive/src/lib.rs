mod shadow;

#[proc_macro_attribute]
pub fn shadow(
    attr: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    proc_macro::TokenStream::from(shadow::shadow(attr.into(), input.into()))
}

#[proc_macro_attribute]
pub fn shadow_patch(
    attr: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    proc_macro::TokenStream::from(shadow::shadow_patch::shadow_patch(
        attr.into(),
        input.into(),
    ))
}
