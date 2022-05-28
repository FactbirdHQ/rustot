use serde::{de::DeserializeOwned, Serialize};

use super::ShadowDiff;

macro_rules! impl_shadow_diff {
    ($($ident: ty),+) => {
        $(
            impl ShadowDiff for $ident {
                type PartialState = Option<$ident>;

                fn apply_patch(&mut self, opt: Self::PartialState) {
                    if let Some(v) = opt {
                        *self = v;
                    }
                }
            }
        )+
    };
}

// Rust primitive types: https://doc.rust-lang.org/reference/types.html
impl_shadow_diff!(bool);
impl_shadow_diff!(u8, u16, u32, u64, u128, usize);
impl_shadow_diff!(i8, i16, i32, i64, i128, isize);
impl_shadow_diff!(f32, f64);
impl_shadow_diff!(char);

// Heapless stuff
impl<const N: usize> ShadowDiff for heapless::String<N> {
    type PartialState = Option<heapless::String<N>>;

    fn apply_patch(&mut self, opt: Self::PartialState) {
        if let Some(v) = opt {
            *self = v;
        }
    }
}

impl<T: Clone + Serialize + DeserializeOwned, const N: usize> ShadowDiff for heapless::Vec<T, N> {
    type PartialState = Option<heapless::Vec<T, N>>;

    fn apply_patch(&mut self, opt: Self::PartialState) {
        if let Some(v) = opt {
            *self = v;
        }
    }
}
