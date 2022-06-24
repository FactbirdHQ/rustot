mod _impl;

use serde::{de::DeserializeOwned, Serialize};

pub trait ShadowPatch: Serialize {
    type PatchState: Serialize + DeserializeOwned + Default + Clone;

    fn apply_patch(&mut self, opt: Self::PatchState);
}
