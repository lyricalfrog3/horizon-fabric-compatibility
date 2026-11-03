#![forbid(unsafe_code)]

pub mod bridge;
pub mod builtins;
pub mod class;
pub mod heap;
pub mod interp;
pub mod jimage;
pub mod loader;
pub mod minecraft;
pub mod value;

pub use bridge::FabricModMetadata;
