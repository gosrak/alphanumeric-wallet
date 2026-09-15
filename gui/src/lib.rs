//! Pure core for the alphanumeric GUI wallet.
//!
//! Everything that can lose funds lives here, and nothing here imports `iced`:
//! seed derivation, transaction signing, the photo secret, and the keystore
//! envelope are all pure functions pinned by test vectors. The GUI crate's
//! binary is added separately and depends on this.
//!
//! This crate deliberately does NOT depend on the `alphanumeric` node crate. It
//! implements `SIGNING_SPEC.md` from the outside, exactly as any third-party
//! signer would, so that the spec itself is exercised.

pub mod activity;
pub mod backend;
pub mod history;
pub mod import;
pub mod keystore;
pub mod model;
pub mod node;
pub mod photo;
pub mod proc;
pub mod seed;
pub mod settings;
pub mod startup;
pub mod storage;
pub mod tx;
