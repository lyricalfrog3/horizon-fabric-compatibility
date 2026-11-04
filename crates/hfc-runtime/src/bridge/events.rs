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

    /// Dispatches tooltip color queries to Iceberg `RenderTooltipEvents.COLOREXT` listeners.
    pub fn item_tooltip_color(
        &mut self,
        item_id: Option<&str>,
        background: &mut u32,
        border_top: &mut u32,
        border_bottom: &mut u32,
    ) {
        let color_event = self.interp.resolve_static_field(
            "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents",
            "COLOREXT",
        );
        if let Some(event_id) = color_event.as_obj() {
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
                        let stack_val = if let Some(id) = item_id {
                            let sid = self.interp.heap.alloc_item_stack(ItemStack::new(id, 1));
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
                                if let Some(JavaObject::Instance { fields, .. }) =
                                    self.interp.heap.get(res_id)
                                {
                                    if let Some(bg) =
                                        fields.get("backgroundStart").map(|v| v.as_int())
                                    {
                                        *background = bg as u32;
                                    }
                                    if let Some(bt) = fields.get("borderStart").map(|v| v.as_int())
                                    {
                                        *border_top = bt as u32;
                                    }
                                    if let Some(bb) = fields.get("borderEnd").map(|v| v.as_int()) {
                                        *border_bottom = bb as u32;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
