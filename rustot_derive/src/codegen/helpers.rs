//! Shared helper functions for code generation

use proc_macro2::TokenStream;
use quote::quote;

/// Build a const expression that computes the maximum of multiple values at compile time.
///
/// Takes an iterator of token streams (each representing a `usize` expression) and produces
/// a single const expression that evaluates to their maximum.
///
/// If the iterator is empty, returns a fallback value (typically `0` or a base value).
///
/// # Example output
/// ```ignore
/// {
///     const fn const_max(a: usize, b: usize) -> usize {
///         if a > b { a } else { b }
///     }
///     const_max(const_max(A, B), C)
/// }
/// ```
pub fn build_const_max_expr(
    items: impl IntoIterator<Item = TokenStream>,
    fallback: TokenStream,
) -> TokenStream {
    let items: Vec<_> = items.into_iter().collect();

    if items.is_empty() {
        return fallback;
    }

    let mut expr = items[0].clone();
    for item in &items[1..] {
        expr = quote! { const_max(#expr, #item) };
    }

    quote! {
        {
            const fn const_max(a: usize, b: usize) -> usize {
                if a > b { a } else { b }
            }
            #expr
        }
    }
}

/// Build a const expression for MAX_KEY_LEN that includes a base value.
///
/// This is a convenience wrapper around `build_const_max_expr` that handles
/// the common pattern of `const_max(base, max(items...))`.
pub fn build_max_key_len_expr(
    items: impl IntoIterator<Item = TokenStream>,
    base: TokenStream,
) -> TokenStream {
    let items: Vec<_> = items.into_iter().collect();

    if items.is_empty() {
        return base;
    }

    let mut expr = items[0].clone();
    for item in &items[1..] {
        expr = quote! { const_max(#expr, #item) };
    }

    quote! {
        {
            const fn const_max(a: usize, b: usize) -> usize {
                if a > b { a } else { b }
            }
            const_max(#base, #expr)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_const_max_expr_empty() {
        let result = build_const_max_expr(std::iter::empty(), quote! { 0 });
        assert_eq!(result.to_string(), "0");
    }

    #[test]
    fn test_build_const_max_expr_single() {
        let items = vec![quote! { 42 }];
        let result = build_const_max_expr(items, quote! { 0 });
        // Should wrap single item in const_max block
        assert!(result.to_string().contains("const fn const_max"));
        assert!(result.to_string().contains("42"));
    }

    #[test]
    fn test_build_const_max_expr_multiple() {
        let items = vec![quote! { A }, quote! { B }, quote! { C }];
        let result = build_const_max_expr(items, quote! { 0 });
        let s = result.to_string();
        assert!(s.contains("const fn const_max"));
        assert!(s.contains("const_max (const_max (A , B) , C)"));
    }
}
