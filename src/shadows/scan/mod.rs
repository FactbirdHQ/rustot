//! JSON scanning utilities for shadow delta processing.
//!
//! This module provides lightweight, `no_std`/`no_alloc` JSON scanning tools
//! for extracting field byte ranges without full parsing.

mod tagged_json;

pub use tagged_json::TaggedJsonScan;
