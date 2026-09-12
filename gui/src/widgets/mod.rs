//! Canvas widgets ported from noid_gui (Apache-2.0, Copyright (C) 2026
//! Paranoid Zero).
//!
//! Only the backdrop came across. `photo_scanner` needs a progress value this
//! wallet does not produce and has no indeterminate mode; `secret_arrow` needs
//! a step layout this wallet's setup flow does not share. `state_field` draws
//! noid's segment atlas, and this chain has neither segments nor UTXOs. They
//! are ported when something here actually needs them -- a widget with no
//! caller reads as a shipped feature.

pub mod interface_backdrop;
