#![forbid(unsafe_code)]

pub mod bridge;
pub mod builtins;
pub mod class;
pub mod fs_builtins;
pub mod heap;
pub mod interp;
pub mod jimage;
pub mod loader;
pub mod minecraft;
pub mod mixin;
pub mod num_builtins;
pub mod reflect;
pub mod registry;
pub mod text_io;
pub mod value;
pub mod vfs;

pub use bridge::FabricModMetadata;
