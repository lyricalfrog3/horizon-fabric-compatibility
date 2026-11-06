//! The open Fabric screen: initializing and rendering it, the client tick, key
//! mappings, and mouse, key and character input.

use super::*;

impl FabricBridge {
    /// The screen a mod installed through `Minecraft.gui.setScreen` (or
    /// `Minecraft.setScreen`), if any.
    pub fn current_screen(&self) -> Option<Value> {
        let mc = self
            .interp
            .heap
            .get_singleton("net/minecraft/client/Minecraft")?;
        let JavaObject::Instance { fields, .. } = self.interp.heap.get(mc)? else {
            return None;
        };
        fields
            .get("screen")
            .filter(|value| !value.is_null())
            .filter(|value| value.as_obj() != self.menu_screen)
            .cloned()
    }

    pub fn has_screen(&self) -> bool {
        self.current_screen().is_some()
    }

    /// Run `Screen.init(width, height)` when the open screen or GUI size
    /// changed, then `ScreenEvents.AFTER_INIT` for the new screen.
    pub(super) fn prepare_screen(&mut self, width: i32, height: i32) -> Result<Option<Value>> {
        let Some(screen) = self.current_screen() else {
            self.screen_id = None;
            return Ok(None);
        };
        let screen_id = screen.as_obj();
        if self.screen_id != screen_id || self.screen_width != width || self.screen_height != height
        {
            self.interp.execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "init",
                "(II)V",
                &[screen.clone(), Value::Int(width), Value::Int(height)],
            )?;
            self.screen_id = screen_id;
            self.screen_width = width;
            self.screen_height = height;
            self.fire_screen_after_init(&screen, width, height);
        }
        Ok(Some(screen))
    }

    pub(super) fn fire_screen_after_init(&mut self, screen: &Value, width: i32, height: i32) {
        let event = self.interp.resolve_static_field(
            "net/fabricmc/fabric/api/client/screen/v1/ScreenEvents",
            "AFTER_INIT",
        );
        let Some(event_id) = event.as_obj() else {
            return;
        };
        let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) else {
            return;
        };
        let Some(Value::Object(Some(arr_id))) = fields.get("listeners") else {
            return;
        };
        let arr_id = *arr_id;
        let listeners =
            if let Some(JavaObject::Array { elements, .. }) = self.interp.heap.get(arr_id) {
                elements.clone()
            } else {
                return;
            };
        let mc = self
            .interp
            .heap
            .get_singleton("net/minecraft/client/Minecraft");
        for listener in listeners {
            let _ = self
                .interp
                .with_budget(super::ENTRYPOINT_BUDGET, |interp| {
                    interp.execute_method(
                        "net/fabricmc/fabric/api/client/screen/v1/ScreenEvents$AfterInit",
                        "afterInit",
                        "(Lnet/minecraft/client/Minecraft;Lnet/minecraft/client/gui/screens/Screen;II)V",
                        &[
                            listener,
                            mc.map(|id| Value::Object(Some(id))).unwrap_or(Value::NULL),
                            screen.clone(),
                            Value::Int(width),
                            Value::Int(height),
                        ],
                    )
                });
        }
    }

    /// Replay the open screen's `GuiGraphicsExtractor` display list. Returns
    /// `None` when no mod screen is open.
    pub fn render_screen(
        &mut self,
        width: i32,
        height: i32,
        partial_tick: f32,
    ) -> Result<Option<Vec<GuiDraw>>> {
        let Some(screen) = self.prepare_screen(width, height)? else {
            return Ok(None);
        };
        let graphics = self
            .interp
            .heap
            .alloc_native(GuiGraphics::new(width, height));
        self.interp
            .with_budget(super::ENTRYPOINT_BUDGET, |interp| {
                interp.execute_method(
                    "net/minecraft/client/gui/screens/Screen",
                    "extractRenderStateWithTooltipAndSubtitles",
                    "(Lnet/minecraft/client/gui/GuiGraphicsExtractor;IIF)V",
                    &[
                        screen,
                        Value::Object(Some(graphics)),
                        Value::Int(0),
                        Value::Int(0),
                        Value::Float(partial_tick),
                    ],
                )
            })?;
        let draws = self
            .interp
            .heap
            .get_native::<GuiGraphics>(graphics)
            .map(|g| g.draws.clone())
            .unwrap_or_default();
        Ok(Some(draws))
    }

    /// `Screen.tick()`, plus Fabric's `ClientTickEvents.END_CLIENT_TICK`.
    pub fn client_tick(&mut self) {
        if let Some(screen) = self.current_screen() {
            let _ = self.interp.execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "tick",
                "()V",
                &[screen],
            );
        }
        let event = self.interp.resolve_static_field(
            "net/fabricmc/fabric/api/client/event/lifecycle/v1/ClientTickEvents",
            "END_CLIENT_TICK",
        );
        let Some(event_id) = event.as_obj() else {
            return;
        };
        let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) else {
            return;
        };
        let Some(Value::Object(Some(arr_id))) = fields.get("listeners") else {
            return;
        };
        let arr_id = *arr_id;
        let listeners =
            if let Some(JavaObject::Array { elements, .. }) = self.interp.heap.get(arr_id) {
                elements.clone()
            } else {
                return;
            };
        let mc = self
            .interp
            .heap
            .get_singleton("net/minecraft/client/Minecraft");
        for listener in listeners {
            let _ = self.interp.execute_method(
                "net/fabricmc/fabric/api/client/event/lifecycle/v1/ClientTickEvents$EndClientTick",
                "onEndTick",
                "(Lnet/minecraft/client/Minecraft;)V",
                &[
                    listener,
                    mc.map(|id| Value::Object(Some(id))).unwrap_or(Value::NULL),
                ],
            );
        }
    }

    /// Trigger a Fabric key mapping by its translation key (the host's binding
    /// action), then run the client tick that consumes it.
    pub fn press_key_mapping(&mut self, name: &str) -> bool {
        let Some(registry) = self.interp.heap.get_singleton("hfc:fabric_key_mappings") else {
            return false;
        };
        let Some(JavaObject::List(items)) = self.interp.heap.get(registry) else {
            return false;
        };
        let mappings = items.clone();
        let mut pressed = false;
        for mapping in mappings {
            let Some(id) = mapping.as_obj() else {
                continue;
            };
            let matches = self
                .interp
                .heap
                .get(id)
                .and_then(|obj| match obj {
                    JavaObject::Instance { fields, .. } => fields.get("name"),
                    _ => None,
                })
                .and_then(Value::as_obj)
                .and_then(|name_id| self.interp.heap.get_string(name_id))
                .is_some_and(|mapping_name| mapping_name == name);
            if matches {
                if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get_mut(id) {
                    let count = fields.get("clickCount").map(|v| v.as_int()).unwrap_or(0);
                    fields.insert("clickCount".into(), Value::Int(count + 1));
                }
                pressed = true;
            }
        }
        if pressed {
            self.client_tick();
        }
        pressed
    }

    /// Press every Fabric mapping bound to a host key code (GLFW key codes).
    pub fn press_key_code(&mut self, key_code: i32) -> bool {
        let Some(registry) = self.interp.heap.get_singleton("hfc:fabric_key_mappings") else {
            return false;
        };
        let Some(JavaObject::List(items)) = self.interp.heap.get(registry) else {
            return false;
        };
        let mappings = items.clone();
        let mut pressed = false;
        for mapping in mappings {
            let Some(id) = mapping.as_obj() else {
                continue;
            };
            let bound = self
                .interp
                .heap
                .get(id)
                .and_then(|obj| match obj {
                    JavaObject::Instance { fields, .. } => fields.get("key"),
                    _ => None,
                })
                .map(|v| v.as_int())
                .unwrap_or(-1);
            if bound == key_code {
                if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get_mut(id) {
                    let count = fields.get("clickCount").map(|v| v.as_int()).unwrap_or(0);
                    fields.insert("clickCount".into(), Value::Int(count + 1));
                }
                pressed = true;
            }
        }
        if pressed {
            self.client_tick();
        }
        pressed
    }

    /// The 26.3 `MouseButtonEvent` a screen dispatch carries.
    pub(super) fn mouse_event(&mut self, x: f64, y: f64, button: i32) -> Value {
        let info = self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/input/MouseButtonInfo".into(),
            fields: [
                ("button".to_string(), Value::Int(button)),
                ("modifiers".to_string(), Value::Int(0)),
            ]
            .into(),
        });
        let event = self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/input/MouseButtonEvent".into(),
            fields: [
                ("x".to_string(), Value::Double(x)),
                ("y".to_string(), Value::Double(y)),
                ("buttonInfo".to_string(), Value::Object(Some(info))),
            ]
            .into(),
        });
        Value::Object(Some(event))
    }

    pub fn mouse_moved(&mut self, x: f64, y: f64) {
        let (dx, dy) = (x - self.last_mouse.0, y - self.last_mouse.1);
        self.last_mouse = (x, y);
        let Some(screen) = self.current_screen() else {
            return;
        };
        if let Some(button) = self.drag_button {
            // `MouseHandler.onMove` with an active button dispatches
            // `mouseDragged`, not `mouseMoved`: a scrollbar thumb or slider
            // only follows the pointer through the drag, while the wheel
            // reaches `mouseScrolled`.
            let event = self.mouse_event(x, y, button);
            let _ = self.interp.execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "mouseDragged",
                "(Lnet/minecraft/client/input/MouseButtonEvent;DD)Z",
                &[screen, event, Value::Double(dx), Value::Double(dy)],
            );
        } else {
            let _ = self.interp.execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "mouseMoved",
                "(DD)V",
                &[screen, Value::Double(x), Value::Double(y)],
            );
        }
    }

    pub fn mouse_button(&mut self, x: f64, y: f64, button: i32, pressed: bool) -> bool {
        let Some(screen) = self.current_screen() else {
            return false;
        };
        self.last_mouse = (x, y);
        let event = self.mouse_event(x, y, button);
        let (method, descriptor, mut args) = if pressed {
            (
                "mouseClicked",
                "(Lnet/minecraft/client/input/MouseButtonEvent;Z)Z",
                vec![screen, event, Value::from_bool(false)],
            )
        } else {
            (
                "mouseReleased",
                "(Lnet/minecraft/client/input/MouseButtonEvent;)Z",
                vec![screen, event],
            )
        };
        args.truncate(if pressed { 3 } else { 2 });
        let handled = self
            .interp
            .execute_method(
                "net/minecraft/client/gui/screens/Screen",
                method,
                descriptor,
                &args,
            )
            .map(|v| v.as_bool())
            .unwrap_or(false);
        if pressed {
            self.drag_button = Some(button);
        } else if self.drag_button == Some(button) {
            self.drag_button = None;
        }
        handled
    }

    pub fn mouse_scrolled(&mut self, x: f64, y: f64, dx: f64, dy: f64) -> bool {
        let Some(screen) = self.current_screen() else {
            return false;
        };
        self.interp
            .execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "mouseScrolled",
                "(DDDD)Z",
                &[
                    screen,
                    Value::Double(x),
                    Value::Double(y),
                    Value::Double(dx),
                    Value::Double(dy),
                ],
            )
            .map(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn key_event(&mut self, key: i32, scancode: i32, modifiers: i32, pressed: bool) -> bool {
        let Some(screen) = self.current_screen() else {
            return false;
        };
        let event = self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/input/KeyEvent".into(),
            fields: [
                ("key".to_string(), Value::Int(key)),
                ("keycode".to_string(), Value::Int(scancode)),
                ("modifiers".to_string(), Value::Int(modifiers)),
            ]
            .into(),
        });
        let method = if pressed { "keyPressed" } else { "keyReleased" };
        self.interp
            .execute_method(
                "net/minecraft/client/gui/screens/Screen",
                method,
                "(Lnet/minecraft/client/input/KeyEvent;)Z",
                &[screen, Value::Object(Some(event))],
            )
            .map(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn char_event(&mut self, codepoint: i32) -> bool {
        let Some(screen) = self.current_screen() else {
            return false;
        };
        let event = self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/input/CharacterEvent".into(),
            fields: [("codepoint".to_string(), Value::Int(codepoint))].into(),
        });
        self.interp
            .execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "charTyped",
                "(Lnet/minecraft/client/input/CharacterEvent;)Z",
                &[screen, Value::Object(Some(event))],
            )
            .map(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// `Screen.onClose`, which usually installs the screen that was open
    /// before. The host leaves Fabric screen mode once `current_screen` is
    /// empty again.
    pub fn close_screen(&mut self) {
        if let Some(screen) = self.current_screen() {
            let _ = self.interp.execute_method(
                "net/minecraft/client/gui/screens/Screen",
                "onClose",
                "()V",
                &[screen],
            );
        }
        self.screen_id = None;
        self.drag_button = None;
    }
}
