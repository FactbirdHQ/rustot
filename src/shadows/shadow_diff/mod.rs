mod _impl;

use serde::{de::DeserializeOwned, Serialize};

pub trait ShadowDiff: Serialize {
    type PartialState: Serialize + DeserializeOwned + Default + Clone;

    fn apply_patch(&mut self, opt: Self::PartialState);
}
