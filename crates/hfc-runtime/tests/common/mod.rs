//! Locating the machine's Minecraft installation for tests that load real classes.
//!
//! The discovery lives in the library (`hfc_runtime::discovery`) so the census
//! example and the tests use one implementation.
#![allow(dead_code, unused_imports)]
pub use hfc_runtime::discovery::{bridge_with_vanilla, image, jars, minecraft_dir, version_jar};
