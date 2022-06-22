use serde::{de::DeserializeOwned, Serialize};

use crate::shadows::data_types::Patch;

use super::ShadowPatch;

macro_rules! impl_shadow_patch {
    ($($ident: ty),+) => {
        $(
            impl ShadowPatch for $ident {
                type PatchState = $ident;

                fn apply_patch(&mut self, opt: Self::PatchState) {
                    *self = opt;
                }
            }
        )+
    };
}

// Rust primitive types: https://doc.rust-lang.org/reference/types.html
impl_shadow_patch!(bool);
impl_shadow_patch!(u8, u16, u32, u64, u128, usize);
impl_shadow_patch!(i8, i16, i32, i64, i128, isize);
impl_shadow_patch!(f32, f64);
impl_shadow_patch!(char);

impl<T: DeserializeOwned + Serialize + Clone> ShadowPatch for Option<T> {
    type PatchState = Patch<T>;

    fn apply_patch(&mut self, opt: Self::PatchState) {
        if let Patch::Set(v) = opt {
            *self = Some(v);
        } else {
            *self = None;
        }
    }
}

// Heapless stuff
impl<const N: usize> ShadowPatch for heapless::String<N> {
    type PatchState = heapless::String<N>;

    fn apply_patch(&mut self, opt: Self::PatchState) {
        *self = opt;
    }
}

impl<T: Clone + Serialize + DeserializeOwned, const N: usize> ShadowPatch for heapless::Vec<T, N> {
    type PatchState = heapless::Vec<T, N>;

    fn apply_patch(&mut self, opt: Self::PatchState) {
        *self = opt;
    }
}
