//! Regression test for externally-tagged enums that mix a unit variant with a
//! newtype variant (the `FlowMode { Custom, Managed(..) }` shape used by
//! factbird-edge flow-manager).
//!
//! serde serializes the UNIT variant as a bare JSON string (`"custom"`) but the
//! newtype variant as an object (`{"managed": {..}}`). The generated
//! `parse_delta` scans for object keys, so before this fix a bare-string unit
//! variant failed with `ParseError::Scan(Expected '{' ...)` — breaking every
//! delta / accepted-response that carried `mode:"custom"`.

#![cfg(all(feature = "std", feature = "shadows_kv_persist"))]
#![allow(async_fn_in_trait)]
#![allow(incomplete_features)]
#![feature(generic_const_exprs)]

use postcard::experimental::max_size::MaxSize;
use rustot::shadows::{NullResolver, ShadowNode};
use rustot_derive::shadow_node;
use serde::{Deserialize, Serialize};

#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
pub struct Template {
    pub id: u32,
    pub version: u32,
}

/// Mirror of flow-manager's `FlowMode`: externally-tagged, `rename_all`, a unit
/// default variant plus a newtype variant.
#[shadow_node]
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, MaxSize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Custom,
    Managed(Template),
}

#[tokio::test]
async fn unit_variant_parses_from_bare_string() {
    let r = NullResolver;

    // The form serde actually emits for the unit variant. Regressed before fix.
    <Mode as ShadowNode>::parse_delta(b"\"custom\"", "", &r)
        .await
        .expect("bare-string unit variant must parse");

    // Newtype variant (object form) must still parse.
    <Mode as ShadowNode>::parse_delta(br#"{"managed":{"id":1,"version":2}}"#, "", &r)
        .await
        .expect("newtype variant object form must parse");

    // Unknown bare string is a clean error, not a panic or Scan failure.
    assert!(
        <Mode as ShadowNode>::parse_delta(b"\"bogus\"", "", &r)
            .await
            .is_err()
    );
}
