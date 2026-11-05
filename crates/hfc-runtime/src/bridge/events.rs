//! Fabric API events the host dispatches: right-click on a block, item tooltips
//! and tooltip colors.

use super::*;

impl FabricBridge {
    pub(super) fn sync_level_to_host(&self, level_id: usize, host: &mut dyn HostBridge) {
        if let Some(final_level) = self.interp.heap.get_native::<Level>(level_id) {
            for (pos, new_state) in &final_level.replanted {
                host.set_block_property(
                    [pos.x, pos.y, pos.z],
                    "age",
                    &new_state.get_age().to_string(),
                );
            }
            for (pos, item, count) in &final_level.dropped_stacks {
                host.drop_item([pos.x, pos.y, pos.z], item, *count as u32);
            }
        }
    }

    /// Dispatch Fabric's `PlayerBlockBreakEvents.BEFORE` for a block the player
    /// is about to break. Listeners may remove further blocks (`destroyBlock`);
    /// those are handed to the host through [`HostBridge::break_block`]. The
    /// level the listeners see is a snapshot of the host's blocks within
    /// `radius` of `pos`, since a listener cannot call back into the host.
    pub fn block_break_before(
        &mut self,
        host: &mut dyn HostBridge,
        pos: [i32; 3],
        crouching: bool,
        radius: i32,
    ) -> Flow {
        if !self.initialized {
            let _ = self.init();
        }
        let block_id = host.block(pos);
        if block_id == "minecraft:air" || block_id.is_empty() {
            return Flow::Continue;
        }
        let mut level = Level::new(false);
        for dx in -radius..=radius {
            for dy in -radius..=radius {
                for dz in -radius..=radius {
                    let at = [pos[0] + dx, pos[1] + dy, pos[2] + dz];
                    let id = host.block(at);
                    if id != "minecraft:air" && !id.is_empty() {
                        level.blocks.insert(at, BlockState::new(&id));
                    }
                }
            }
        }
        let state = level.blocks[&pos].clone();
        let level_id = self.interp.heap.alloc_level(level);
        let mut player = Player::new(level_id);
        player.is_crouching = crouching;
        player.server_side = true;
        player.hand_item = host.held_item().map(|id| {
            let mut stack = ItemStack::new(id, 1);
            stack.enchantments = host.held_enchantments();
            stack
        });
        let player_id = self.interp.heap.alloc_player(player);
        let pos_id = self.interp.heap.alloc_block_pos(pos[0], pos[1], pos[2]);
        let state_id = self.interp.heap.alloc_block_state(state);

        // The mod registered on the field the interface's own initializer
        // created; resolve it the same way `getstatic` does.
        let _ = self
            .interp
            .ensure_initialized("net/fabricmc/fabric/api/event/player/PlayerBlockBreakEvents");
        let event = self.interp.resolve_static_field(
            "net/fabricmc/fabric/api/event/player/PlayerBlockBreakEvents",
            "BEFORE",
        );
        let mut listeners = Vec::new();
        if let Some(JavaObject::Instance { fields, .. }) =
            event.as_obj().and_then(|id| self.interp.heap.get(id))
        {
            if let Some(Value::Object(Some(arr))) = fields.get("listeners") {
                if let Some(JavaObject::Array { elements, .. }) = self.interp.heap.get(*arr) {
                    listeners.extend(elements.clone());
                }
            }
        }
        let mut flow = Flow::Continue;
        let prev_budget = self.interp.step_budget;
        self.interp.step_budget = Some(ENTRYPOINT_BUDGET);
        for listener in listeners {
            let result = self.interp.execute_method(
                "net/fabricmc/fabric/api/event/player/PlayerBlockBreakEvents$Before",
                "beforeBlockBreak",
                "(Lnet/minecraft/world/level/Level;Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/block/entity/BlockEntity;)Z",
                &[
                    listener,
                    Value::Object(Some(level_id)),
                    Value::Object(Some(player_id)),
                    Value::Object(Some(pos_id)),
                    Value::Object(Some(state_id)),
                    Value::NULL,
                ],
            );
            match result {
                Ok(value) if value.as_int() == 0 => {
                    flow = Flow::Cancel;
                    break;
                }
                Ok(_) => {}
                Err(error) => log::warn!("PlayerBlockBreakEvents.BEFORE listener failed: {error}"),
            }
        }
        self.interp.run_server_tasks();
        self.interp.step_budget = prev_budget;
        let broken = self
            .interp
            .heap
            .get_native::<Level>(level_id)
            .map(|level| level.broken.clone())
            .unwrap_or_default();
        for at in broken {
            if at.x != pos[0] || at.y != pos[1] || at.z != pos[2] {
                host.break_block([at.x, at.y, at.z]);
            }
        }
        flow
    }

    /// The listeners registered on a Fabric `Event` held in a static field.
    fn event_listeners(&mut self, class_name: &str, field: &str) -> Vec<Value> {
        // Resolve the field the way `getstatic` does: the interface's own
        // initializer creates the Event the mods registered on.
        let _ = self.interp.ensure_initialized(class_name);
        let event = self.interp.resolve_static_field(class_name, field);
        let mut listeners = Vec::new();
        if let Some(JavaObject::Instance { fields, .. }) =
            event.as_obj().and_then(|id| self.interp.heap.get(id))
        {
            if let Some(Value::Object(Some(arr))) = fields.get("listeners") {
                if let Some(JavaObject::Array { elements, .. }) = self.interp.heap.get(*arr) {
                    listeners.extend(elements.clone());
                }
            }
        }
        listeners
    }

    /// Tell mods the game's (integrated) server is starting, as
    /// `ServerLifecycleEvents.SERVER_STARTING` does. Server-side mods load
    /// their configuration here.
    pub fn server_starting(&mut self) {
        if !self.initialized {
            let _ = self.init();
        }
        let server = self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/server/MinecraftServer".into(),
            fields: HashMap::new(),
        });
        let prev_budget = self.interp.step_budget;
        self.interp.step_budget = Some(ENTRYPOINT_BUDGET);
        let listeners = self.event_listeners(
            "net/fabricmc/fabric/api/event/lifecycle/v1/ServerLifecycleEvents",
            "SERVER_STARTING",
        );
        for listener in listeners {
            self.interp.step_budget = Some(ENTRYPOINT_BUDGET);
            let result = self.interp.execute_method(
                "net/fabricmc/fabric/api/event/lifecycle/v1/ServerLifecycleEvents$ServerStarting",
                "onServerStarting",
                "(Lnet/minecraft/server/MinecraftServer;)V",
                &[listener, Value::Object(Some(server))],
            );
            if let Err(error) = result {
                log::warn!("ServerLifecycleEvents.SERVER_STARTING listener failed: {error}");
            }
        }
        self.interp.run_server_tasks();
        self.interp.step_budget = prev_budget;
    }

    /// Dispatch a right-click block event into the dynamic Fabric compatibility layer.
    pub fn right_click_block(
        &mut self,
        host: &mut dyn HostBridge,
        event: &RightClickBlockEvent,
    ) -> Flow {
        if !self.initialized {
            let _ = self.init();
        }

        let block_id = host.block(event.pos);
        if block_id == "minecraft:air" || block_id.is_empty() {
            return Flow::Continue;
        }

        // 1. Prepare initial BlockState from Host
        let mut block_state = BlockState::new(&block_id);
        if let Some(age_str) = host.block_property(event.pos, "age") {
            block_state = block_state.with_property("age", &age_str);
        }

        // 2. Prepare Level and BlockPos on heap
        let mut level = Level::new(false);
        let block_pos = BlockPos {
            x: event.pos[0],
            y: event.pos[1],
            z: event.pos[2],
        };
        level
            .blocks
            .insert([block_pos.x, block_pos.y, block_pos.z], block_state);

        let level_id = self.interp.heap.alloc_level(level);
        let player = Player::new(level_id);
        let player_id = self.interp.heap.alloc_player(player);

        let hit_dir = Direction::from_array(event.face);
        let hit_id = self
            .interp
            .heap
            .alloc_hit_result(block_pos.clone(), hit_dir.clone());
        let pos_id = self
            .interp
            .heap
            .alloc_block_pos(block_pos.x, block_pos.y, block_pos.z);
        let dir_id = self.interp.heap.alloc_direction(hit_dir);
        let main_hand = self
            .interp
            .resolve_static_field("net/minecraft/world/InteractionHand", "MAIN_HAND");

        // 3. Dispatch to UseBlockCallback.EVENT listeners (Fabric API)
        let use_block_event = self.interp.resolve_static_field(
            "net/fabricmc/fabric/api/event/player/UseBlockCallback",
            "EVENT",
        );
        if let Some(event_id) = use_block_event.as_obj() {
            if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) {
                if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                    let arr_id = *arr_id;
                    let listeners = if let Some(JavaObject::Array { elements, .. }) =
                        self.interp.heap.get(arr_id)
                    {
                        elements.clone()
                    } else {
                        Vec::new()
                    };
                    for listener in listeners {
                        let res = self.interp.execute_method(
                            "net/fabricmc/fabric/api/event/player/UseBlockCallback",
                            "interact",
                            "(Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/level/Level;Lnet/minecraft/world/InteractionHand;Lnet/minecraft/world/phys/BlockHitResult;)Lnet/minecraft/world/InteractionResult;",
                            &[
                                listener,
                                Value::Object(Some(player_id)),
                                Value::Object(Some(level_id)),
                                main_hand.clone(),
                                Value::Object(Some(hit_id)),
                            ],
                        );
                        if let Ok(val) = res {
                            let is_pass = if let Some(id) = val.as_obj() {
                                if let Some(JavaObject::Instance { fields, .. }) =
                                    self.interp.heap.get(id)
                                {
                                    fields
                                        .get("name")
                                        .and_then(|n| n.as_obj())
                                        .and_then(|nid| self.interp.heap.get_string(nid))
                                        .map(|s| s == "PASS")
                                        .unwrap_or(false)
                                } else {
                                    false
                                }
                            } else {
                                val.is_null()
                            };
                            if !is_pass {
                                self.sync_level_to_host(level_id, host);
                                return Flow::Cancel;
                            }
                        }
                    }
                }
            }
        }

        // 4. Dispatch to InteractionEvent.USE_BLOCK listeners (JamLib / mod events)
        let jamlib_use_block = self.interp.resolve_static_field(
            "io/github/jamalam360/jamlib/api/events/InteractionEvent",
            "USE_BLOCK",
        );
        if let Some(event_id) = jamlib_use_block.as_obj() {
            let mut listeners = Vec::new();
            if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) {
                if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                    let arr_id = *arr_id;
                    if let Some(JavaObject::Array { elements, .. }) = self.interp.heap.get(arr_id) {
                        listeners.extend(elements.clone());
                    }
                }
                if let Some(single) = fields.get("listener") {
                    if !single.is_null() && !listeners.contains(single) {
                        listeners.push(single.clone());
                    }
                }
            }
            for listener in listeners {
                let res = self.interp.execute_method(
                    "io/github/jamalam360/jamlib/api/events/InteractionEvent$UseBlock",
                    "onUseBlock",
                    "(Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/InteractionHand;Lnet/minecraft/core/BlockPos;Lnet/minecraft/core/Direction;)Lio/github/jamalam360/jamlib/api/events/core/EventResult;",
                    &[
                        listener,
                        Value::Object(Some(player_id)),
                        main_hand.clone(),
                        Value::Object(Some(pos_id)),
                        Value::Object(Some(dir_id)),
                    ],
                );
                if let Ok(Value::Object(Some(res_id))) = res {
                    if let Some(event_res) = self.interp.heap.get_native::<EventResult>(res_id) {
                        if event_res.cancelled {
                            self.sync_level_to_host(level_id, host);
                            return Flow::Cancel;
                        }
                    }
                }
            }
        }

        Flow::Continue
    }

    /// Dispatches item tooltip gathering to Fabric `ItemTooltipCallback` listeners.
    pub fn item_tooltip(&mut self, item_id: &str, count: u32, lines: &mut Vec<(String, u32)>) {
        let tooltip_event = self.interp.resolve_static_field(
            "net/fabricmc/fabric/api/client/item/v1/ItemTooltipCallback",
            "EVENT",
        );
        if let Some(event_id) = tooltip_event.as_obj() {
            if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(event_id) {
                if let Some(Value::Object(Some(arr_id))) = fields.get("listeners") {
                    let arr_id = *arr_id;
                    let listeners = if let Some(JavaObject::Array { elements, .. }) =
                        self.interp.heap.get(arr_id)
                    {
                        elements.clone()
                    } else {
                        Vec::new()
                    };

                    if !listeners.is_empty() {
                        let stack_id = self
                            .interp
                            .heap
                            .alloc_item_stack(ItemStack::new(item_id, count as i32));
                        let stack_val = Value::Object(Some(stack_id));

                        let mut comp_vals = Vec::new();
                        for (text_str, col) in lines.iter() {
                            let str_id = self.interp.heap.alloc_string(text_str.clone());
                            let mut cf = HashMap::new();
                            cf.insert("text".into(), Value::Object(Some(str_id)));
                            cf.insert("color".into(), Value::Int(*col as i32));
                            let cid = self.interp.heap.alloc(JavaObject::Instance {
                                class_name: "net/minecraft/network/chat/MutableComponent".into(),
                                fields: cf,
                            });
                            comp_vals.push(Value::Object(Some(cid)));
                        }
                        let list_id = self.interp.heap.alloc_list(comp_vals);
                        let list_val = Value::Object(Some(list_id));

                        for listener in listeners {
                            let _ = self.interp.execute_method(
                                "net/fabricmc/fabric/api/client/item/v1/ItemTooltipCallback",
                                "getTooltip",
                                "(Lnet/minecraft/world/item/ItemStack;Lnet/minecraft/world/item/Item$TooltipContext;Lnet/minecraft/world/item/TooltipFlag;Ljava/util/List;)V",
                                &[listener, stack_val.clone(), Value::NULL, Value::NULL, list_val.clone()],
                            );
                        }

                        if let Some(JavaObject::List(items)) = self.interp.heap.get(list_id) {
                            let mut new_lines = Vec::new();
                            for item in items {
                                if let Some(cid) = item.as_obj() {
                                    let (line_str, col) = if let Some(s) =
                                        self.interp.heap.get_string(cid)
                                    {
                                        (s.to_string(), u32::MAX)
                                    } else if let Some(JavaObject::Instance { fields, .. }) =
                                        self.interp.heap.get(cid)
                                    {
                                        let s = fields
                                            .get("text")
                                            .and_then(|v| v.as_obj())
                                            .and_then(|sid| self.interp.heap.get_string(sid))
                                            .unwrap_or("")
                                            .to_string();
                                        let c = fields
                                            .get("color")
                                            .map(|v| v.as_int() as u32)
                                            .unwrap_or(u32::MAX);
                                        (s, c)
                                    } else {
                                        (String::new(), u32::MAX)
                                    };
                                    new_lines.push((line_str, col));
                                }
                            }
                            *lines = new_lines;
                        }
                    }
                }
            }
        }
    }

    /// The client's `Font`, as `Minecraft.getInstance().font`.
    fn minecraft_font(&mut self) -> Value {
        let minecraft = self
            .interp
            .execute_method(
                "net/minecraft/client/Minecraft",
                "getInstance",
                "()Lnet/minecraft/client/Minecraft;",
                &[],
            )
            .ok();
        minecraft
            .and_then(|value| value.as_obj())
            .and_then(|id| match self.interp.heap.get(id) {
                Some(JavaObject::Instance { fields, .. }) => fields.get("font").cloned(),
                _ => None,
            })
            .unwrap_or(Value::NULL)
    }

    /// Dispatches tooltip color queries to Iceberg `RenderTooltipEvents.COLOREXT` listeners.
    pub fn item_tooltip_color(
        &mut self,
        item_id: Option<&str>,
        rarity: Option<&str>,
        background: &mut u32,
        border_top: &mut u32,
        border_bottom: &mut u32,
    ) {
        let listeners = self.event_listeners(
            "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents",
            "COLOREXT",
        );
        if listeners.is_empty() {
            return;
        }
        let stack_val = if let Some(id) = item_id {
            let sid = self
                .interp
                .heap
                .alloc_item_stack(ItemStack::new(id, 1).with_rarity(rarity));
            Value::Object(Some(sid))
        } else {
            Value::NULL
        };

        let bg_start = Value::Int(*background as i32);
        let bg_end = Value::Int(*background as i32);
        let border_start = Value::Int(*border_top as i32);
        let border_end = Value::Int(*border_bottom as i32);

        for listener in listeners {
            let res = self.interp.execute_method(
                "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents$ColorExt",
                "onColor",
                "(Lnet/minecraft/world/item/ItemStack;Lnet/minecraft/client/gui/GuiGraphicsExtractor;IILnet/minecraft/client/gui/Font;IIIILjava/util/List;ZILnet/minecraft/resources/Identifier;ZZ)Lcom/anthonyhilyard/iceberg/events/client/RenderTooltipEvents$ColorExtResult;",
                &[
                    listener,
                    stack_val.clone(),
                    Value::NULL,
                    Value::Int(0),
                    Value::Int(0),
                    Value::NULL,
                    bg_start.clone(),
                    bg_end.clone(),
                    border_start.clone(),
                    border_end.clone(),
                    Value::NULL,
                    Value::from_bool(false),
                    Value::Int(0),
                    Value::NULL,
                    Value::from_bool(false),
                    Value::from_bool(false),
                ],
            );

            if let Ok(Value::Object(Some(res_id))) = res {
                if let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get(res_id) {
                    if let Some(bg) = fields.get("backgroundStart").map(|v| v.as_int()) {
                        *background = bg as u32;
                    }
                    if let Some(bt) = fields.get("borderStart").map(|v| v.as_int()) {
                        *border_top = bt as u32;
                    }
                    if let Some(bb) = fields.get("borderEnd").map(|v| v.as_int()) {
                        *border_bottom = bb as u32;
                    }
                }
            }
        }
    }

    /// Dispatches the tooltip's post-render draw to Iceberg
    /// `RenderTooltipEvents.POSTEXT` listeners and returns what they recorded
    /// into a `GuiGraphicsExtractor` display list. Minecraft calls this at the
    /// tail of its tooltip pass, after the background, frame and text, which is
    /// where a mod draws a custom frame (`TooltipDecor.drawBorder`) and shadow.
    /// `x`/`y` are the tooltip positioner's text origin and `width`/`height`
    /// the tooltip's text extent, the same values vanilla passes.
    #[allow(clippy::too_many_arguments)]
    pub fn item_tooltip_frame(
        &mut self,
        item_id: &str,
        rarity: Option<&str>,
        screen_width: i32,
        screen_height: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Vec<GuiDraw> {
        if !self.initialized {
            let _ = self.init();
        }
        let listeners = self.event_listeners(
            "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents",
            "POSTEXT",
        );
        if listeners.is_empty() {
            return Vec::new();
        }
        let stack = self
            .interp
            .heap
            .alloc_item_stack(ItemStack::new(item_id, 1).with_rarity(rarity));
        let graphics = self
            .interp
            .heap
            .alloc_native(GuiGraphics::new(screen_width, screen_height));
        let font = self.minecraft_font();
        let components = self.interp.heap.alloc_list(Vec::new());

        let prev_budget = self.interp.step_budget;
        self.interp.step_budget = Some(ENTRYPOINT_BUDGET);
        for listener in listeners {
            self.interp.step_budget = Some(ENTRYPOINT_BUDGET);
            let result = self.interp.execute_method(
                "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents$PostExt",
                "onPost",
                "(Lnet/minecraft/world/item/ItemStack;Lnet/minecraft/client/gui/GuiGraphicsExtractor;IILnet/minecraft/client/gui/Font;IILjava/util/List;ZIZ)V",
                &[
                    listener,
                    Value::Object(Some(stack)),
                    Value::Object(Some(graphics)),
                    Value::Int(x),
                    Value::Int(y),
                    font.clone(),
                    Value::Int(width),
                    Value::Int(height),
                    Value::Object(Some(components)),
                    Value::from_bool(false),
                    Value::Int(0),
                    Value::from_bool(false),
                ],
            );
            if let Err(error) = result {
                log::warn!("RenderTooltipEvents.POSTEXT listener failed: {error}");
            }
        }
        self.interp.step_budget = prev_budget;
        self.interp
            .heap
            .get_native::<GuiGraphics>(graphics)
            .map(|g| g.draws.clone())
            .unwrap_or_default()
    }
}
