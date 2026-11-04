//! Images a mod registered at run time, and GUI sprites it ships in its own jar.

use super::*;

impl FabricBridge {
    /// Textures mods registered with `TextureManager.register`, keyed by
    /// identifier. The host decodes and draws them; the runtime has no GPU.
    pub fn registered_textures(&self) -> &HashMap<String, crate::minecraft::PngImage> {
        &self.interp.heap.textures
    }

    /// The image a GUI blit names, when no host atlas has it. A texture a mod
    /// registered at run time wins; otherwise `namespace:path` is read from a
    /// loaded mod's own resources the way a resource pack would be:
    /// `textures/...png` identifiers map to `assets/<namespace>/<path>` and
    /// sprite identifiers (`blitSprite`) to
    /// `assets/<namespace>/textures/gui/sprites/<path>.png`. Misses are cached
    /// so a per-frame lookup does not reopen jars.
    pub fn gui_texture(&mut self, identifier: &str) -> Option<crate::minecraft::PngImage> {
        if let Some(image) = self.interp.heap.textures.get(identifier) {
            return Some(image.clone());
        }
        if let Some(cached) = self.gui_texture_cache.get(identifier) {
            return cached.clone();
        }
        let found = self.read_gui_texture(identifier);
        self.gui_texture_cache
            .insert(identifier.to_string(), found.clone());
        found
    }

    pub(super) fn read_gui_texture(&self, identifier: &str) -> Option<crate::minecraft::PngImage> {
        let (namespace, path) = identifier.split_once(':')?;
        if namespace == "minecraft" {
            return None;
        }
        let entry = if path.ends_with(".png") {
            format!("assets/{namespace}/{path}")
        } else {
            format!("assets/{namespace}/textures/gui/sprites/{path}.png")
        };
        for meta in &self.mods {
            let Some(jar) = meta.jar.as_deref() else {
                continue;
            };
            let Some(png) = mixin::read_mod_resource(jar, meta.nested_jar.as_deref(), &entry)
            else {
                continue;
            };
            // IHDR: width and height are the big-endian words at 16 and 20.
            if png.len() < 24 || !png.starts_with(b"\x89PNG") {
                continue;
            }
            let word =
                |at: usize| u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
            return Some(crate::minecraft::PngImage {
                width: word(16),
                height: word(20),
                png,
            });
        }
        None
    }
}
