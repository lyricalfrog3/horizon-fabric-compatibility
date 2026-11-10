#![forbid(unsafe_code)]

pub mod block_builtins;
pub mod block_entity_builtins;
pub mod bridge;
pub mod builtins;
pub mod census;
pub mod class;
pub mod container_builtins;
pub mod creative_tab_builtins;
pub mod discovery;
pub mod fs_builtins;
pub mod heap;
pub mod interp;
pub mod item_builtins;
pub mod jimage;
pub mod loader;
pub mod menu_builtins;
pub mod minecraft;
pub mod mixin;
pub mod num_builtins;
pub mod place_builtins;
pub mod reflect;
pub mod registry;
pub mod text_io;
pub mod use_builtins;
pub mod value;
pub mod value_io_builtins;
pub mod vfs;

pub use bridge::FabricModMetadata;
