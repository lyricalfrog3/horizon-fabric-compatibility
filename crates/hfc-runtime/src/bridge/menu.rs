//! Widgets a mod's mixins add to host menus: reading the mixin configs, running
//! the widget-adding `@Inject` callbacks with synthesized locals, drawing and
//! clicking the widgets they record.

use super::*;

impl FabricBridge {
    /// Read every loaded mod's mixin configs and the injectors their client
    /// mixin classes declare. Only the menu-widget `@Inject` subset is applied;
    /// everything else is recorded in [`MixinManifest::unsupported`].
    pub(super) fn load_mixin_manifest(&mut self) -> MixinManifest {
        let mut manifest = MixinManifest::default();
        let mods = self.mods.clone();
        for meta in &mods {
            let Some(jar) = meta.jar.as_deref() else {
                continue;
            };
            for entry in mixin::config_entries(&meta.raw) {
                if entry.environment == "server" {
                    continue;
                }
                let Some(bytes) =
                    mixin::read_mod_resource(jar, meta.nested_jar.as_deref(), &entry.config)
                else {
                    manifest.unsupported.push(format!(
                        "{}: missing mixin config {}",
                        meta.id, entry.config
                    ));
                    continue;
                };
                let config = String::from_utf8_lossy(&bytes);
                for class_name in mixin::config_classes(&config) {
                    if self.interp.loader.get(&class_name).is_none() {
                        if let Err(error) = self.interp.loader.get_or_load(&class_name) {
                            manifest.unsupported.push(format!("{class_name}: {error}"));
                            continue;
                        }
                    }
                    let Some(class) = self.interp.loader.get(&class_name) else {
                        continue;
                    };
                    manifest
                        .accessors
                        .extend(mixin::accessors_from_class(class));
                    for injector in mixin::injectors_from_class(class) {
                        if injector.supported() {
                            manifest.injectors.push(injector);
                        } else {
                            manifest.unsupported.push(format!(
                                "{}.{}{} -> {}.{}: {:?}",
                                injector.mixin_class,
                                injector.callback_method,
                                injector.callback_descriptor,
                                injector.target_class,
                                injector.target_method,
                                injector.point
                            ));
                        }
                    }
                }
            }
        }
        manifest
    }

    /// Whether any applied injector targets this host menu.
    pub fn has_menu_injectors(&self, kind: MenuKind) -> bool {
        let class_name = kind.screen_class();
        let method_name = kind.layout_method();
        self.mixins.injectors.iter().any(|injector| {
            injector.supported()
                && injector.target_class == class_name
                && injector.target_method == method_name
        })
    }

    /// Run the host menu's real layout method with the mods' menu injectors
    /// active and return the widgets they added. Results are cached per GUI
    /// size; the host calls this whenever the menu opens or resizes.
    pub fn prepare_menu_contributions(
        &mut self,
        kind: MenuKind,
        width: i32,
        height: i32,
    ) -> Vec<MenuWidget> {
        let key = (kind, width, height);
        if !self.menu_cache.contains_key(&key) {
            let mut ids = self.run_menu_layout(kind, width, height);
            ids.retain(|id| self.is_menu_widget(*id));
            self.menu_cache.insert(key, ids);
        }
        self.menu_cache
            .get(&key)
            .into_iter()
            .flatten()
            .filter_map(|id| self.menu_widget(*id))
            .collect()
    }

    pub(super) fn run_menu_layout(
        &mut self,
        kind: MenuKind,
        width: i32,
        height: i32,
    ) -> Vec<usize> {
        if !self.has_menu_injectors(kind) {
            return Vec::new();
        }
        let class_name = kind.screen_class();
        let method_name = kind.layout_method();
        let mut fields = HashMap::new();
        fields.insert("width".into(), Value::Int(width));
        fields.insert("height".into(), Value::Int(height));
        if let Some(minecraft) = self
            .interp
            .heap
            .get_singleton("net/minecraft/client/Minecraft")
        {
            fields.insert("minecraft".into(), Value::Object(Some(minecraft)));
        }
        let screen = self.interp.heap.alloc(JavaObject::Instance {
            class_name: class_name.into(),
            fields,
        });
        self.menu_screen = Some(screen);
        let injectors: Vec<crate::mixin::MixinInjector> = self
            .mixins
            .injectors
            .iter()
            .filter(|injector| {
                injector.supported()
                    && injector.target_class == class_name
                    && injector.target_method == method_name
            })
            .cloned()
            .collect();
        self.interp.menu_contributions.clear();
        self.interp.menu_injection = true;
        for injector in &injectors {
            self.run_menu_callback(injector, screen);
        }
        self.interp.menu_injection = false;
        std::mem::take(&mut self.interp.menu_contributions)
    }

    /// Call one menu mixin callback with a synthetic target-screen receiver,
    /// `CallbackInfo`, and synthesized layout parameters. The vanilla layout
    /// method is not run: the callback only builds and adds its widgets, and
    /// the host places them (see `MenuWidget`).
    pub(super) fn run_menu_callback(
        &mut self,
        injector: &crate::mixin::MixinInjector,
        screen: usize,
    ) {
        if self.interp.loader.get(&injector.mixin_class).is_none() {
            let _ = self.interp.loader.get_or_load(&injector.mixin_class);
        }
        let is_static = self
            .interp
            .loader
            .get(&injector.mixin_class)
            .and_then(|class| {
                class.methods.get(&(
                    injector.callback_method.clone(),
                    injector.callback_descriptor.clone(),
                ))
            })
            .map(|method| method.is_static)
            .unwrap_or(false);
        let info = self
            .interp
            .heap
            .alloc_native(crate::minecraft::CallbackInfo::default());
        let types = crate::mixin::descriptor_parameter_types(&injector.callback_descriptor);
        let mut args = Vec::with_capacity(types.len() + 2);
        if !is_static {
            args.push(Value::Object(Some(screen)));
        }
        args.push(Value::Object(Some(info)));
        for parameter in types.iter().skip(1) {
            args.push(self.synthesize_menu_parameter(parameter, screen));
        }
        self.interp.injection_depth = 1;
        let result = self.interp.execute_method(
            &injector.mixin_class,
            &injector.callback_method,
            &injector.callback_descriptor,
            &args,
        );
        self.interp.injection_depth = 0;
        if let Err(error) = result {
            log::warn!(
                "Menu callback {}.{}{} failed: {error}",
                injector.mixin_class,
                injector.callback_method,
                injector.callback_descriptor
            );
        }
    }

    pub(super) fn synthesize_menu_parameter(&mut self, descriptor: &str, screen: usize) -> Value {
        let name = descriptor
            .strip_prefix('L')
            .and_then(|name| name.strip_suffix(';'))
            .unwrap_or(descriptor);
        match descriptor {
            "I" | "Z" | "B" | "S" | "C" => Value::Int(0),
            "J" => Value::Long(0),
            "F" => Value::Float(0.0),
            "D" => Value::Double(0.0),
            _ if name.contains("mixinextras/sugar/ref/Local") => {
                // MixinExtras `@Local` sugar wrappers: give the callback a
                // mutable box it can read and write. The values are the host's
                // synthesized ones (zero/false), not the vanilla layout's
                // counters; the callback only needs to build its widget.
                let value = if name.ends_with("IntRef") {
                    Value::Int(0)
                } else if name.ends_with("LongRef") {
                    Value::Long(0)
                } else if name.ends_with("FloatRef") {
                    Value::Float(0.0)
                } else if name.ends_with("DoubleRef") {
                    Value::Double(0.0)
                } else if name.ends_with("BooleanRef") {
                    // A menu callback's boolean is usually its "add the
                    // widget" gate; false would suppress every contribution.
                    Value::Int(1)
                } else {
                    Value::NULL
                };
                let mut fields = HashMap::new();
                fields.insert("value".into(), value);
                let id = self.interp.heap.alloc(JavaObject::Instance {
                    class_name: name.to_string(),
                    fields,
                });
                Value::Object(Some(id))
            }
            _ if name.ends_with("Layout")
                || name.ends_with("RowHelper")
                || name.ends_with("GridLayout") =>
            {
                let id = self.interp.heap.alloc(JavaObject::Instance {
                    class_name: name.to_string(),
                    fields: HashMap::new(),
                });
                Value::Object(Some(id))
            }
            _ if name == "net/minecraft/client/gui/screens/Screen" || name.ends_with("/Screen") => {
                Value::Object(Some(screen))
            }
            _ if name == "net/minecraft/client/Minecraft" => self
                .interp
                .heap
                .get_singleton("net/minecraft/client/Minecraft")
                .map(|id| Value::Object(Some(id)))
                .unwrap_or(Value::NULL),
            _ => Value::NULL,
        }
    }

    /// True when `id` is (or extends) a widget with a render-state method.
    pub(super) fn is_menu_widget(&mut self, id: usize) -> bool {
        let Some(class_name) = self
            .interp
            .heap
            .get(id)
            .map(|object| object.class_name().to_string())
        else {
            return false;
        };
        let mut queue = vec![class_name];
        let mut visited = std::collections::HashSet::new();
        while let Some(current) = queue.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if self.interp.loader.get(&current).is_none() {
                let _ = self.interp.loader.get_or_load(&current);
            }
            let Some(class) = self.interp.loader.get(&current) else {
                continue;
            };
            if class.methods.keys().any(|(name, descriptor)| {
                (name == "extractRenderState" || name == "extractWidgetRenderState")
                    && descriptor.starts_with("(Lnet/minecraft/client/gui/GuiGraphicsExtractor;")
            }) {
                return true;
            }
            if let Some(super_class) = &class.super_class {
                queue.push(super_class.clone());
            }
            queue.extend(class.interfaces.iter().cloned());
        }
        false
    }

    /// One contributed widget's public description.
    pub(super) fn menu_widget(&self, id: usize) -> Option<MenuWidget> {
        let JavaObject::Instance { fields, .. } = self.interp.heap.get(id)? else {
            return None;
        };
        let get = |name: &str| fields.get(name).map(Value::as_int);
        let label = fields
            .get("message")
            .map(|message| crate::builtins::text_of_value(&self.interp.heap, message))
            .unwrap_or_default();
        Some(MenuWidget {
            id,
            label,
            x: get("x").unwrap_or(0),
            y: get("y").unwrap_or(0),
            width: get("width").unwrap_or(0),
            height: get("height").unwrap_or(0),
            visible: get("visible").map(|value| value != 0).unwrap_or(true),
            active: get("active").map(|value| value != 0).unwrap_or(true),
        })
    }

    /// The contributed widget under a GUI point for a prepared menu size.
    pub fn menu_widget_at(
        &self,
        kind: MenuKind,
        width: i32,
        height: i32,
        x: f64,
        y: f64,
    ) -> Option<usize> {
        let ids = self.menu_cache.get(&(kind, width, height))?;
        for (index, id) in ids.iter().enumerate() {
            let Some(widget) = self.menu_widget(*id) else {
                continue;
            };
            if widget.visible
                && widget.active
                && x >= f64::from(widget.x)
                && y >= f64::from(widget.y)
                && x < f64::from(widget.x + widget.width)
                && y < f64::from(widget.y + widget.height)
            {
                return Some(index);
            }
        }
        None
    }

    /// Run one contributed widget's real render extraction.
    pub fn menu_widget_draw(
        &mut self,
        kind: MenuKind,
        width: i32,
        height: i32,
        index: usize,
        mouse_x: i32,
        mouse_y: i32,
    ) -> Vec<GuiDraw> {
        let Some(id) = self
            .menu_cache
            .get(&(kind, width, height))
            .and_then(|ids| ids.get(index).copied())
        else {
            return Vec::new();
        };
        let graphics = self
            .interp
            .heap
            .alloc_native(GuiGraphics::new(width, height));
        let result = self.interp.execute_method(
            "net/minecraft/client/gui/components/AbstractWidget",
            "extractRenderState",
            "(Lnet/minecraft/client/gui/GuiGraphicsExtractor;IIF)V",
            &[
                Value::Object(Some(id)),
                Value::Object(Some(graphics)),
                Value::Int(mouse_x),
                Value::Int(mouse_y),
                Value::Float(0.0),
            ],
        );
        if let Err(error) = result {
            log::warn!("Menu widget render failed: {error}");
            return Vec::new();
        }
        self.interp
            .heap
            .get_native::<GuiGraphics>(graphics)
            .map(|graphics| graphics.draws.clone())
            .unwrap_or_default()
    }

    /// Click or release a contributed widget.
    pub fn menu_widget_click(
        &mut self,
        kind: MenuKind,
        width: i32,
        height: i32,
        index: usize,
        x: f64,
        y: f64,
        button: i32,
        pressed: bool,
    ) -> bool {
        let Some(id) = self
            .menu_cache
            .get(&(kind, width, height))
            .and_then(|ids| ids.get(index).copied())
        else {
            return false;
        };
        let event = self.mouse_event(x, y, button);
        let (method, descriptor, args) = if pressed {
            (
                "mouseClicked",
                "(Lnet/minecraft/client/input/MouseButtonEvent;Z)Z",
                vec![Value::Object(Some(id)), event, Value::from_bool(false)],
            )
        } else {
            (
                "mouseReleased",
                "(Lnet/minecraft/client/input/MouseButtonEvent;)Z",
                vec![Value::Object(Some(id)), event],
            )
        };
        self.interp
            .execute_method(
                "net/minecraft/client/gui/components/AbstractWidget",
                method,
                descriptor,
                &args,
            )
            .map(|value| value.as_bool())
            .unwrap_or(false)
    }
}
