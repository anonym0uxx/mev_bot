//! Credential and configuration types.
//!
//! Design rules that follow from the types:
//! - A constructed URL containing a key is `Secret`, never `String`.
//! - Every log, span field, error message, and metric label uses the
//!   redacted form.
//! - No `unwrap_or`, no `unwrap_or_default`, no baked-in fallback
//!   anywhere in credential resolution. Absent means refuse to start.

pub mod creds;
pub mod secret;

pub use creds::Creds;
pub use secret::Secret;
