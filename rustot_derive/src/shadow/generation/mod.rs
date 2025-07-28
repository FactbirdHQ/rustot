pub mod generator;
pub mod modifier;
pub mod variant_or_field_visitor;

use generator::Generator;
use modifier::Modifier;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{punctuated::Punctuated, spanned::Spanned as _, Data, DeriveInput, Path, Token};
use variant_or_field_visitor::{
    borrow_fields_mut, get_attr, has_shadow_arg, is_primitive, VariantOrFieldVisitor,
};

use crate::shadow::generation::variant_or_field_visitor::borrow_fields;

use super::CFG_ATTRIBUTE;

pub struct ShadowGenerator {
    input: DeriveInput,
    output: DeriveInput,
    generated: TokenStream,
}

impl ShadowGenerator {
    pub fn new(input: DeriveInput) -> Self {
        let output = input.clone();
        Self {
            input,
            output,
            generated: quote! {},
        }
    }

    pub fn variant_or_field_visitor(mut self, visitor: &mut impl VariantOrFieldVisitor) -> Self {
        match (&mut self.input.data, &mut self.output.data) {
            (Data::Struct(data_struct_old), Data::Struct(data_struct_new)) => {
                let old_fields = borrow_fields_mut(data_struct_old);
                let new_fields = borrow_fields_mut(data_struct_new);

                for (old_field, new_field) in old_fields.iter_mut().zip(new_fields.iter_mut()) {
                    visitor.visit_field(old_field, new_field);
                }
            }
            (Data::Enum(data_enum_old), Data::Enum(data_enum_new)) => {
                for (old_variant, new_variant) in data_enum_old
                    .variants
                    .iter_mut()
                    .zip(data_enum_new.variants.iter_mut())
                {
                    visitor.visit_variant(old_variant, new_variant);
                }
            }
            _ => {}
        }

        self
    }

    pub fn modifier(mut self, modifier: &mut impl Modifier) -> Self {
        modifier.modify(&self.input, &mut self.output);

        self
    }

    pub fn generator(mut self, generator: &mut impl Generator) -> Self {
        let gen = generator.generate(&self.input, &self.output);

        let generated = self.generated;
        self.generated = quote! {
            #generated

            #gen
        };

        self
    }

    pub fn finalize(self) -> TokenStream {
        self.generated
    }
}

pub struct GenerateShadowPatchImplVisitor {
    field_index: usize,
    reported_ident: Path,
    apply_patch_impl: TokenStream,
}

impl GenerateShadowPatchImplVisitor {
    pub fn new(reported_ident: Path) -> Self {
        Self {
            field_index: 0,
            reported_ident,
            apply_patch_impl: quote! {},
        }
    }

    fn variables_actions<'a>(
        fields: impl Iterator<Item = &'a syn::Field>,
    ) -> (
        Punctuated<syn::Ident, Token![,]>,
        Punctuated<TokenStream, Token![,]>,
    ) {
        fields.enumerate().fold(
            (Punctuated::new(), Punctuated::new()),
            |(mut variables, mut actions), (i, field)| {
                let var_ident = field.ident.clone().unwrap_or_else(|| {
                    syn::Ident::new(&format!("{}", (b'a' + i as u8) as char), field.span())
                });

                let action = if is_primitive(&field.ty) || has_shadow_arg(&field.attrs, "leaf") {
                    quote! {Some(#var_ident)}
                } else {
                    quote! {Some(#var_ident.into_reported())}
                };

                actions.push(
                    field
                        .ident
                        .as_ref()
                        .map(|ident| quote! {#ident: #action})
                        .unwrap_or(action),
                );

                variables.push(var_ident);

                (variables, actions)
            },
        )
    }
}

impl Generator for GenerateShadowPatchImplVisitor {
    fn generate(&mut self, original: &DeriveInput, output: &DeriveInput) -> TokenStream {
        let (impl_generics, ty_generics, where_clause) = original.generics.split_for_impl();
        let orig_name = &original.ident;
        let delta_name = &output.ident;
        let reported_name = &self.reported_ident;

        let apply_patch_impl = match original.data {
            Data::Enum(_) => {
                let arms = &self.apply_patch_impl;
                quote! {
                    match (self, delta) {
                        #arms
                    }
                }
            }
            _ => {
                let implementation = &self.apply_patch_impl;
                quote! { #implementation }
            }
        };

        let into_reported_impl = match (&original.data, &output.data) {
            (Data::Struct(data_struct_old), Data::Struct(data_struct_new)) => {
                let original_fields = borrow_fields(data_struct_old);
                let new_fields = borrow_fields(data_struct_new);

                let from_fields = original_fields.iter().fold(quote! {}, |acc, field| {
                    let is_leaf = is_primitive(&field.ty) || has_shadow_arg(&field.attrs, "leaf");

                    let has_new_field = new_fields
                        .iter()
                        .find(|&f| f.ident == field.ident)
                        .is_some();

                    let cfg_attr = get_attr(&field.attrs, CFG_ATTRIBUTE);

                    let ident = &field.ident;
                    if !has_new_field {
                        quote! { #acc #cfg_attr #ident: None, }
                    } else if is_leaf {
                        quote! { #acc #cfg_attr #ident: Some(self.#ident), }
                    } else {
                        quote! { #acc #cfg_attr #ident: Some(self.#ident.into_reported()), }
                    }
                });

                quote! {
                    Self::Reported {
                        #from_fields
                    }
                }
            }
            (Data::Enum(data_struct_old), Data::Enum(_)) => {
                let match_arms = data_struct_old
                            .variants
                            .iter()
                            .fold(Punctuated::<TokenStream, Token![,]>::new(), |mut acc, variant| {
                                let variant_ident = &variant.ident;
                                let cfg_attr = get_attr(&variant.attrs, CFG_ATTRIBUTE);

                                acc.push(match &variant.fields {
                                    syn::Fields::Named(fields_named) => {
                                        let (variables, actions) = Self::variables_actions(fields_named.named.iter());
                                        quote! {#cfg_attr #orig_name::#variant_ident { #variables } => Self::Reported::#variant_ident { #actions }}
                                    }
                                    syn::Fields::Unnamed(fields_unnamed) => {
                                        let (variables, actions) = Self::variables_actions(fields_unnamed.unnamed.iter());
                                        quote! {#cfg_attr #orig_name::#variant_ident ( #variables ) => Self::Reported::#variant_ident ( #actions )}
                                    }
                                    syn::Fields::Unit => {
                                        quote! {#cfg_attr #orig_name::#variant_ident => Self::Reported::#variant_ident}
                                    }
                                });

                                acc
                            });

                quote! {
                    match self {
                        #match_arms
                    }
                }
            }
            _ => panic!(),
        };

        quote! {
            impl #impl_generics rustot::shadows::ShadowPatch for #orig_name #ty_generics #where_clause {
                type Delta = #delta_name #ty_generics;
                type Reported = #reported_name #ty_generics;

                fn apply_patch(&mut self, delta: Self::Delta) {
                    #apply_patch_impl
                }

                fn into_reported(self) -> Self::Reported {
                    #into_reported_impl
                }
            }
        }
    }
}

#[derive(Default)]
struct PatchImpl {
    variables: Punctuated<TokenStream, Token![,]>,
    delta_variables: Punctuated<TokenStream, Token![,]>,
    delta_deconstructors: Punctuated<TokenStream, Token![,]>,
    defaults: TokenStream,
    assigns: TokenStream,
}

impl PatchImpl {
    fn new<'a>(fields: impl Iterator<Item = &'a syn::Field>) -> PatchImpl {
        fields
            .enumerate()
            .fold(PatchImpl::default(), |mut patch_impl, (i, field)| {
                let cfg_attr = get_attr(&field.attrs, CFG_ATTRIBUTE);

                let var_ident = field
                    .ident
                    .clone()
                    .unwrap_or_else(|| format_ident!("{}", (b'a' + i as u8) as char));

                let delta_ident = format_ident!("d_{}", var_ident);

                let cfg_var = quote! { #cfg_attr #var_ident};
                let cfg_delta = quote! { #cfg_attr #delta_ident};

                let assigns = patch_impl.assigns;
                let defaults = patch_impl.defaults;
                let field_ty = &field.ty;

                let (action, action_deref) =
                    if has_shadow_arg(&field.attrs, "leaf") || is_primitive(&field.ty) {
                        (
                            quote! {
                                #cfg_attr
                                let #var_ident = #delta_ident.unwrap_or_default();
                            },
                            quote! {
                                #cfg_attr
                                if let Some(delta_var) = #delta_ident {
                                    *#var_ident = delta_var;
                                }
                            },
                        )
                    } else {
                        (
                            quote! {
                                #cfg_attr
                                let mut #var_ident = #field_ty ::default();

                                #cfg_attr
                                if let Some(delta_var) = #delta_ident {
                                    #var_ident.apply_patch(delta_var);
                                }
                            },
                            quote! {
                                #cfg_attr
                                if let Some(delta_var) = #delta_ident {
                                    #var_ident.apply_patch(delta_var);
                                }
                            },
                        )
                    };

                patch_impl.defaults = quote! {
                    #defaults

                    #action
                };

                patch_impl.assigns = quote! {
                    #assigns

                    #action_deref
                };

                patch_impl.variables.push(cfg_var);
                patch_impl.delta_variables.push(cfg_delta);
                patch_impl
                    .delta_deconstructors
                    .push(quote! { #var_ident: #delta_ident});

                patch_impl
            })
    }
}

impl VariantOrFieldVisitor for GenerateShadowPatchImplVisitor {
    fn visit_field(&mut self, old: &mut syn::Field, _new: &mut syn::Field) {
        let field_index = self.field_index;
        self.field_index += 1;

        let field_ident = old
            .ident
            .as_ref()
            .map(|i| quote! { #i })
            .unwrap_or_else(|| {
                let i = syn::Index::from(field_index);
                quote! { #i }
            });

        let cfg_attr = get_attr(&old.attrs, CFG_ATTRIBUTE);

        let acc = &self.apply_patch_impl;

        self.apply_patch_impl = if has_shadow_arg(&old.attrs, "leaf") || is_primitive(&old.ty) {
            quote! {
                #acc

                #cfg_attr if let Some(inner) = delta.#field_ident { self.#field_ident = inner; }
            }
        } else {
            quote! {
                #acc

                #cfg_attr if let Some(inner) = delta.#field_ident { self.#field_ident.apply_patch(inner); }
            }
        };
    }

    fn visit_variant(&mut self, old: &mut syn::Variant, _new: &mut syn::Variant) {
        let variant_ident = &old.ident;
        let variant_cfg = get_attr(&old.attrs, CFG_ATTRIBUTE);

        let acc = &self.apply_patch_impl;
        self.apply_patch_impl = match &old.fields {
            syn::Fields::Named(fields_named) => {
                let PatchImpl {
                    variables,
                    delta_deconstructors,
                    defaults,
                    assigns,
                    ..
                } = PatchImpl::new(fields_named.named.iter());

                quote! {
                    #acc

                    #variant_cfg
                    (Self::#variant_ident { #variables }, Self::Delta::#variant_ident { #delta_deconstructors }) => {
                        #assigns
                    }
                    #variant_cfg
                    (this, Self::Delta::#variant_ident { #delta_deconstructors }) => {
                        #defaults

                        *this = Self::#variant_ident { #variables };
                    }
                }
            }
            syn::Fields::Unnamed(fields_unnamed) => {
                let PatchImpl {
                    variables,
                    delta_variables,
                    defaults,
                    assigns,
                    ..
                } = PatchImpl::new(fields_unnamed.unnamed.iter());

                quote! {
                    #acc

                    #variant_cfg
                    (Self::#variant_ident ( #variables ), Self::Delta::#variant_ident ( #delta_variables )) => {
                        #assigns
                    }
                    #variant_cfg
                    (this, Self::Delta::#variant_ident( #delta_variables)) => {
                        #defaults

                        *this = Self::#variant_ident ( #variables );
                    }
                }
            }
            syn::Fields::Unit => {
                quote! {
                    #acc

                    #variant_cfg (this, Self::Delta::#variant_ident) => *this = Self::#variant_ident,
                }
            }
        };
    }
}
