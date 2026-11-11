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
        for (at, drop) in broken {
            if at.x != pos[0] || at.y != pos[1] || at.z != pos[2] {
                if drop {
                    host.break_block([at.x, at.y, at.z]);
                } else {
                    host.remove_block([at.x, at.y, at.z], false);
                }
            }
        }
        flow
    }

    /// Dispatch Iceberg's and Prism's `RenderTickEvents.START` with the real
    /// time since the previous tooltip frame, as `DeltaTracker.getRealtimeDeltaTicks`
    /// (20 ticks per second). Legendary's shine timer and model rotation read it.
    /// The stack object for the tooltip the host shows, and the container
    /// screen whose hovered slot holds it. Both are kept while the host keeps
    /// showing the same tooltip; a different item or text makes new ones, as
    /// hovering another slot gives vanilla another `ItemStack`.
    fn hovered_tooltip_stack(
        &mut self,
        item_id: &str,
        rarity: Option<&str>,
        max_damage: i32,
        lines: &[(String, u32)],
    ) -> (usize, Option<usize>) {
        let key: HoveredTooltipKey = (item_id.to_string(), lines.to_vec());
        if let Some((held, stack, screen)) = &self.hovered_tooltip
            && *held == key
        {
            return (*stack, *screen);
        }
        let stack = self.interp.heap.alloc_item_stack(
            ItemStack::new(item_id, 1)
                .with_rarity(rarity)
                .with_max_damage(max_damage),
        );
        let screen = self.hovered_slot_screen(stack);
        self.hovered_tooltip = Some((key, stack, screen));
        (stack, screen)
    }

    /// A container screen whose `hoveredSlot` is a real `Slot` over a real
    /// one-slot `SimpleContainer` holding `stack`: what `Gui.screen()` is while
    /// the player hovers an item in an inventory. The screen itself is a host
    /// stand-in (its menu and player are not modelled); its slot and container
    /// run their own bytecode. None if the classes cannot be built.
    fn hovered_slot_screen(&mut self, stack: usize) -> Option<usize> {
        const CONTAINER: &str = "net/minecraft/world/SimpleContainer";
        const SLOT: &str = "net/minecraft/world/inventory/Slot";
        self.interp.ensure_initialized(CONTAINER).ok()?;
        self.interp.ensure_initialized(SLOT).ok()?;
        let container = self.interp.alloc_uninitialized(CONTAINER);
        self.interp
            .execute_special_method(
                CONTAINER,
                "<init>",
                "(I)V",
                &[Value::Object(Some(container)), Value::Int(1)],
            )
            .ok()?;
        self.interp
            .execute_method(
                CONTAINER,
                "setItem",
                "(ILnet/minecraft/world/item/ItemStack;)V",
                &[
                    Value::Object(Some(container)),
                    Value::Int(0),
                    Value::Object(Some(stack)),
                ],
            )
            .ok()?;
        let slot = self.interp.alloc_uninitialized(SLOT);
        self.interp
            .execute_special_method(
                SLOT,
                "<init>",
                "(Lnet/minecraft/world/Container;III)V",
                &[
                    Value::Object(Some(slot)),
                    Value::Object(Some(container)),
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(0),
                ],
            )
            .ok()?;
        Some(self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/gui/screens/inventory/AbstractContainerScreen".into(),
            fields: [("hoveredSlot".to_string(), Value::Object(Some(slot)))].into(),
        }))
    }

    /// Set the client's current screen (`Minecraft.screen` and `Gui.screen`)
    /// and return what it was.
    fn swap_client_screen(&mut self, screen: Value) -> Option<(Value, Value)> {
        let minecraft = self
            .interp
            .execute_method(
                "net/minecraft/client/Minecraft",
                "getInstance",
                "()Lnet/minecraft/client/Minecraft;",
                &[],
            )
            .ok()?
            .as_obj()?;
        let (old_minecraft, gui) = match self.interp.heap.get_mut(minecraft) {
            Some(JavaObject::Instance { fields, .. }) => (
                fields
                    .insert("screen".into(), screen.clone())
                    .unwrap_or(Value::NULL),
                fields.get("gui").and_then(|v| v.as_obj()),
            ),
            _ => return None,
        };
        let old_gui = match gui.and_then(|gui| self.interp.heap.get_mut(gui)) {
            Some(JavaObject::Instance { fields, .. }) => fields
                .insert("screen".into(), screen)
                .unwrap_or(Value::NULL),
            _ => Value::NULL,
        };
        Some((old_minecraft, old_gui))
    }

    /// Restore what [`Self::swap_client_screen`] returned.
    fn restore_client_screen(&mut self, old: Option<(Value, Value)>) {
        let Some((old_minecraft, old_gui)) = old else {
            return;
        };
        self.swap_client_screen(old_gui);
        if let Some(minecraft) = self
            .interp
            .heap
            .get_singleton("net/minecraft/client/Minecraft")
            && let Some(JavaObject::Instance { fields, .. }) = self.interp.heap.get_mut(minecraft)
        {
            fields.insert("screen".into(), old_minecraft);
        }
    }

    /// Render-tick events, with the client's screen set to `screen` (the
    /// hovered container slot) while they run.
    fn dispatch_render_ticks(&mut self, screen: Option<usize>) {
        let now = std::time::Instant::now();
        let delta = self
            .last_render_tick
            .map(|previous| now.duration_since(previous).as_secs_f32() * 20.0)
            .unwrap_or(0.0)
            .clamp(0.0, 20.0);
        self.last_render_tick = Some(now);
        if delta <= 0.0 {
            return;
        }
        let tracker = self.interp.heap.alloc(JavaObject::Instance {
            class_name: "net/minecraft/client/DeltaTracker".into(),
            fields: [("delta".to_string(), Value::Float(delta))].into(),
        });
        let previous =
            screen.and_then(|screen| self.swap_client_screen(Value::Object(Some(screen))));
        let _ = self.dispatch_event(
            "com/anthonyhilyard/iceberg/events/client/RenderTickEvents",
            "START",
            "com/anthonyhilyard/iceberg/events/client/RenderTickEvents$Start",
            "onStart",
            "(Lnet/minecraft/client/DeltaTracker;)V",
            &[Value::Object(Some(tracker.clone()))],
        );
        let _ = self.dispatch_event(
            "com/anthonyhilyard/prism/events/client/RenderTickEvent",
            "START",
            "com/anthonyhilyard/prism/events/client/RenderTickEvent$Start",
            "onStart",
            "(Lnet/minecraft/client/DeltaTracker;)V",
            &[Value::Object(Some(tracker))],
        );
        self.restore_client_screen(previous);
    }

    /// Give the component list's text lines the host line colors. A library
    /// that recomposes text (Iceberg's title centring) builds a new formatted
    /// sequence without the original style, so the color would otherwise fall
    /// back to white.
    fn apply_tooltip_line_colors(&mut self, components: usize, lines: &[(String, u32)]) {
        let items = match self.interp.heap.get(components) {
            Some(JavaObject::List(items)) => items.clone(),
            _ => Vec::new(),
        };
        let mut line = 0;
        for item in items {
            let Some(JavaObject::Instance { class_name, fields }) =
                item.as_obj().and_then(|id| self.interp.heap.get(id))
            else {
                continue;
            };
            if !class_name.ends_with("ClientTextTooltip") {
                continue;
            }
            let Some(sequence) = fields.get("text").and_then(|text| text.as_obj()) else {
                line += 1;
                continue;
            };
            let color = lines.get(line).map(|(_, color)| *color as i32);
            if let Some(color) = color {
                if let Some(JavaObject::Instance { fields, .. }) =
                    self.interp.heap.get_mut(sequence)
                {
                    // The host's line color wins: a recomposed title carries a
                    // zero style color, which would draw black on the dark box.
                    fields.insert("color".to_string(), Value::Int(color));
                }
            }
            line += 1;
        }
    }

    /// Call every listener registered on an Iceberg event. Each listener gets
    /// the real interface method and the supplied arguments; the values they
    /// return are in listener order.
    fn dispatch_event(
        &mut self,
        class_name: &str,
        field: &str,
        interface: &str,
        method: &str,
        descriptor: &str,
        args: &[Value],
    ) -> Vec<Value> {
        let listeners = self.event_listeners(class_name, field);
        let mut results = Vec::new();
        let prev_budget = self.interp.step_budget;
        self.interp.step_budget = Some(ENTRYPOINT_BUDGET);
        for listener in listeners {
            self.interp.step_budget = Some(ENTRYPOINT_BUDGET);
            let mut call = Vec::with_capacity(args.len() + 1);
            call.push(listener);
            call.extend_from_slice(args);
            match self
                .interp
                .execute_method(interface, method, descriptor, &call)
            {
                Ok(value) => results.push(value),
                Err(error) => {
                    log::warn!("{interface}.{method} listener failed: {error}");
                }
            }
        }
        self.interp.step_budget = prev_budget;
        results
    }

    /// Run the non-cancellable `@Inject(at = HEAD)` callbacks a mixin adds to
    /// a target method, with the target's real receiver and arguments. The
    /// callbacks can observe or mutate those arguments (for example Legendary
    /// Tooltips' `tooltip` callback centres the title component list). A
    /// cancellable HEAD callback would replace the target method, so it needs
    /// the real method body and is not applied here.
    fn run_head_injectors(
        &mut self,
        target_class: &str,
        target_method: &str,
        target_descriptor: &str,
        receiver: Value,
        params: &[Value],
    ) {
        // Only verified injectors run here. The host seam synthesizes the
        // target's arguments rather than executing its body, so a cancellable
        // callback cannot end a body that is not run; it is applied by the
        // interpreter instead, where the real body exists.
        let injectors: Vec<crate::mixin::WiredInjector> = self
            .mixins
            .wired
            .iter()
            .filter(|injector| {
                !injector.cancellable
                    && injector.point == crate::mixin::InjectionPoint::Head
                    && injector.declaring_class == target_class
                    && injector.target_method == target_method
                    && injector.target_descriptor == target_descriptor
            })
            .cloned()
            .collect();
        for injector in injectors {
            let is_static = injector.callback_static;
            let info = self
                .interp
                .heap
                .alloc_native(crate::minecraft::CallbackInfo::new(
                    target_method,
                    injector.cancellable,
                ));
            let mut args = Vec::with_capacity(params.len() + 2);
            if !is_static {
                args.push(receiver.clone());
            }
            args.extend_from_slice(params);
            args.push(Value::Object(Some(info)));
            let result = self.interp.execute_method(
                &injector.mixin_class,
                &injector.callback_method,
                &injector.callback_descriptor,
                &args,
            );
            if let Err(error) = result {
                log::warn!(
                    "HEAD injector {}.{}{} failed: {error}",
                    injector.mixin_class,
                    injector.callback_method,
                    injector.callback_descriptor
                );
            }
        }
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

    /// Whether an `InteractionResult` a listener returned is `PASS`, which
    /// lets the next listener and then vanilla handle the click. With
    /// Minecraft's classes loaded, `InteractionResult.PASS` is vanilla's
    /// `InteractionResult$Pass` record; without them it is the runtime's
    /// stand-in constant, named `PASS`.
    fn interaction_result_passes(&self, val: &Value) -> bool {
        let Some(id) = val.as_obj() else {
            return val.is_null();
        };
        let Some(JavaObject::Instance { class_name, fields }) = self.interp.heap.get(id) else {
            return false;
        };
        class_name == "net/minecraft/world/InteractionResult$Pass"
            || fields
                .get("name")
                .and_then(|n| n.as_obj())
                .and_then(|nid| self.interp.heap.get_string(nid))
                .is_some_and(|s| s == "PASS")
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
        // The player's own inventory, for the menu a block may open: its
        // player slots read this snapshot.
        self.interp.heap.inventory = host.inventory();

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
        // A real `Vec3` for the click, so `HitResult.getLocation` answers it.
        // Without one, the hit result keeps no location rather than a faked
        // origin.
        let hit_location = self.interp.alloc_vec3(event.hit).ok();
        let hit_id = self.interp.heap.alloc_hit_result_with(
            block_pos.clone(),
            hit_dir.clone(),
            hit_location,
            false,
        );
        let pos_id = self
            .interp
            .heap
            .alloc_block_pos(block_pos.x, block_pos.y, block_pos.z);
        let dir_id = self.interp.heap.alloc_direction(hit_dir);
        // The hand this turn of `startUseItem` is acting with: the host calls
        // this once per hand, and a listener that checks the hand must see
        // the one it is really in.
        let hand = self.interp.resolve_static_field(
            "net/minecraft/world/InteractionHand",
            if event.off_hand {
                "OFF_HAND"
            } else {
                "MAIN_HAND"
            },
        );

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
                                hand.clone(),
                                Value::Object(Some(hit_id)),
                            ],
                        );
                        if let Ok(val) = res {
                            if !self.interaction_result_passes(&val) {
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
                        hand.clone(),
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

        // 5. The block's own useItemOn/useWithoutItem, for a block a mod
        // registered in code (a container block keeps the host's path).
        match self.block_used(host, event) {
            BlockUse::NotOverridden | BlockUse::Passed => {}
            BlockUse::Failed(error) => {
                // A use that changed the held item is refused on every click;
                // say it once per block, not ten times a second.
                if error.contains("changed the held item")
                    && !self.hand_change_logged.insert(block_id.clone())
                {
                    log::debug!("Fabric: {error}");
                } else {
                    log::warn!("Fabric: {block_id}'s use failed: {error}");
                }
            }
            BlockUse::Handled(effects) => {
                self.apply_block_effects(host, &effects);
                self.remember_opened_menu(&block_id, &effects);
                return Flow::Cancel;
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

    /// Build the real `List<ClientTooltipComponent>` for a tooltip, the way
    /// Iceberg's tooltip mixin does: wrap each host line in a
    /// `FormattedText`, then run Iceberg's own `gatherTooltipComponents`.
    /// That method fires `RenderTooltipEvents.GATHER` (where a mod adds its
    /// item-model and padding components), applies any width-driven line
    /// splitting and converts text and image components to their client
    /// implementations.
    #[allow(clippy::too_many_arguments)]
    fn tooltip_component_list(
        &mut self,
        item_id: &str,
        rarity: Option<&str>,
        max_damage: i32,
        lines: &[(String, u32)],
        screen_width: i32,
        screen_height: i32,
        max_width: i32,
    ) -> Option<usize> {
        let stack = self.interp.heap.alloc_item_stack(
            ItemStack::new(item_id, 1)
                .with_rarity(rarity)
                .with_max_damage(max_damage),
        );

        // The raw element list Iceberg passes to GATHER: each text line is a
        // `FormattedText` on the left of an `Either`.
        let mut elements = Vec::with_capacity(lines.len());
        for (text, color) in lines {
            let text_id = self.interp.heap.alloc_string(text.clone());
            let mut fields = HashMap::new();
            fields.insert("text".into(), Value::Object(Some(text_id)));
            fields.insert("color".into(), Value::Int(*color as i32));
            let component = self.interp.heap.alloc(JavaObject::Instance {
                class_name: "net/minecraft/network/chat/MutableComponent".into(),
                fields,
            });
            let either = self
                .interp
                .execute_method(
                    "com/mojang/datafixers/util/Either",
                    "left",
                    "(Ljava/lang/Object;)Lcom/mojang/datafixers/util/Either;",
                    &[Value::Object(Some(component))],
                )
                .unwrap_or(Value::NULL);
            elements.push(either);
        }
        let mut element_list = self.interp.heap.alloc_list(elements);

        let results = self.dispatch_event(
            "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents",
            "GATHER",
            "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents$Gather",
            "onGather",
            "(Lnet/minecraft/world/item/ItemStack;IILjava/util/List;II)Lcom/anthonyhilyard/iceberg/events/client/RenderTooltipEvents$GatherResult;",
            &[
                Value::Object(Some(stack)),
                Value::Int(screen_width),
                Value::Int(screen_height),
                Value::Object(Some(element_list)),
                Value::Int(max_width),
                Value::Int(0),
            ],
        );
        if let Some(result) = results.last().and_then(|value| value.as_obj()) {
            let fields = match self.interp.heap.get(result) {
                Some(JavaObject::Instance { fields, .. }) => Some(fields.clone()),
                _ => None,
            };
            if let Some(fields) = fields {
                // `GatherResult.result()` other than PASS suppresses the tooltip.
                let passes = fields
                    .get("result")
                    .and_then(|value| value.as_obj())
                    .and_then(|id| self.interp.heap.get(id))
                    .map(|object| object.class_name().ends_with("$Pass"))
                    .unwrap_or(true);
                if !passes {
                    return Some(self.interp.heap.alloc_list(Vec::new()));
                }
                if let Some(Value::Object(Some(list))) = fields.get("tooltipElements") {
                    element_list = *list;
                }
            }
        }

        let elements = match self.interp.heap.get(element_list) {
            Some(JavaObject::List(items)) => items.clone(),
            _ => Vec::new(),
        };
        let mut clients = Vec::with_capacity(elements.len());
        for element in elements {
            let Some(object) = element.as_obj().and_then(|id| self.interp.heap.get(id)) else {
                continue;
            };
            let (left, right) = match object {
                JavaObject::Instance { class_name, fields } if class_name.ends_with("$Left") => {
                    (fields.get("value").cloned(), None)
                }
                JavaObject::Instance { class_name, fields } if class_name.ends_with("$Right") => {
                    (None, fields.get("value").cloned())
                }
                // The runtime answers `Either.left/right` with one instance
                // class carrying the side it holds.
                JavaObject::Instance { class_name, fields }
                    if class_name == "com/mojang/datafixers/util/Either" =>
                {
                    (fields.get("left").cloned(), fields.get("right").cloned())
                }
                _ => continue,
            };
            if let Some(text) = left {
                let sequence = self
                    .interp
                    .execute_method(
                        "net/minecraft/network/chat/FormattedText",
                        "getVisualOrderText",
                        "()Lnet/minecraft/util/FormattedCharSequence;",
                        &[text],
                    )
                    .unwrap_or(Value::NULL);
                let client = self
                    .interp
                    .execute_method(
                        "net/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipComponent",
                        "create",
                        "(Lnet/minecraft/util/FormattedCharSequence;)Lnet/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipComponent;",
                        &[sequence],
                    )
                    .unwrap_or(Value::NULL);
                if !client.is_null() {
                    clients.push(client);
                }
            } else if let Some(component) = right {
                let mut client = self
                    .interp
                    .execute_method(
                        "com/anthonyhilyard/iceberg/util/Tooltips",
                        "getClientComponent",
                        "(Lnet/minecraft/world/inventory/tooltip/TooltipComponent;)Lnet/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipComponent;",
                        &[component.clone()],
                    )
                    .unwrap_or(Value::NULL);
                if client.is_null()
                    && self.interp.is_instance_of(
                        &component,
                        "net/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipComponent",
                    )
                {
                    client = component;
                }
                if !client.is_null() {
                    clients.push(client);
                }
            }
        }
        Some(self.interp.heap.alloc_list(clients))
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
            let res = self
                .interp
                .with_budget(super::ENTRYPOINT_BUDGET, |interp| interp.execute_method(
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
            ))
            ;

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

    /// Build the mods' tooltip from their real `ClientTooltipComponent` list:
    /// gather (GATHER), apply the non-cancellable HEAD injectors on
    /// `GuiGraphicsExtractor.tooltip` (Legendary's centred title), measure the
    /// components, record their text and images into a display list, then
    /// append `POSTEXT` (the frame, shadow and separator). The draws are
    /// relative to the tooltip's text origin. Returns `None` when no Iceberg
    /// tooltip event has a listener, so a mod set without one keeps the host's
    /// own tooltip renderer.
    #[allow(clippy::too_many_arguments)]
    pub fn item_tooltip_render(
        &mut self,
        item_id: &str,
        rarity: Option<&str>,
        max_damage: i32,
        lines: &[(String, u32)],
        screen_width: i32,
        screen_height: i32,
    ) -> Option<crate::bridge::TooltipRender> {
        if !self.initialized {
            let _ = self.init();
        }
        let (stack, screen) = self.hovered_tooltip_stack(item_id, rarity, max_damage, lines);
        self.dispatch_render_ticks(screen);
        // COLOREXT also paints the static colors `POSTEXT` reads when it draws
        // the frame, so it must run before the frame is recorded.
        {
            let (mut background, mut border_top, mut border_bottom) =
                (0xf0100010u32, 0x505000ffu32, 0x5028007fu32);
            self.item_tooltip_color(
                Some(item_id),
                rarity,
                &mut background,
                &mut border_top,
                &mut border_bottom,
            );
        }
        let has_tooltip_events = [
            ("GATHER", "Gather"),
            ("COLOREXT", "ColorExt"),
            ("POSTEXT", "PostExt"),
        ]
        .iter()
        .any(|(field, _)| {
            !self
                .event_listeners(
                    "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents",
                    field,
                )
                .is_empty()
        });
        if !has_tooltip_events {
            return None;
        }
        let components = self.tooltip_component_list(
            item_id,
            rarity,
            max_damage,
            lines,
            screen_width,
            screen_height,
            0,
        )?;
        let graphics = self
            .interp
            .heap
            .alloc_native(GuiGraphics::new(screen_width, screen_height));
        let font = self.minecraft_font();
        self.run_head_injectors(
            "net/minecraft/client/gui/GuiGraphicsExtractor",
            "tooltip",
            "(Lnet/minecraft/client/gui/Font;Ljava/util/List;IILnet/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipPositioner;Lnet/minecraft/resources/Identifier;Z)V",
            Value::Object(Some(graphics)),
            &[
                font.clone(),
                Value::Object(Some(components)),
                Value::Int(0),
                Value::Int(0),
                Value::NULL,
                Value::NULL,
                Value::from_bool(false),
            ],
        );
        self.apply_tooltip_line_colors(components, lines);

        let items = match self.interp.heap.get(components) {
            Some(JavaObject::List(items)) => items.clone(),
            _ => Vec::new(),
        };
        let mut width = 0;
        let mut height = 0;
        for item in &items {
            let component_width = self
                .interp
                .execute_method(
                    "net/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipComponent",
                    "getWidth",
                    "(Lnet/minecraft/client/gui/Font;)I",
                    &[item.clone(), font.clone()],
                )
                .map(|value| value.as_int())
                .unwrap_or(0);
            let component_height = self
                .interp
                .execute_method(
                    "net/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipComponent",
                    "getHeight",
                    "(Lnet/minecraft/client/gui/Font;)I",
                    &[item.clone(), font.clone()],
                )
                .map(|value| value.as_int())
                .unwrap_or(0);
            width = width.max(component_width);
            height += component_height;
        }

        // Vanilla draws every component's text, then every component's image,
        // both walking down by each component's height.
        let mut y = 0;
        for item in &items {
            let result = self.interp.execute_method(
                "net/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipComponent",
                "extractText",
                "(Lnet/minecraft/client/gui/GuiGraphicsExtractor;Lnet/minecraft/client/gui/Font;II)V",
                &[
                    item.clone(),
                    Value::Object(Some(graphics)),
                    font.clone(),
                    Value::Int(0),
                    Value::Int(y),
                ],
            );
            if let Err(error) = result {
                log::warn!("tooltip extractText failed: {error}");
            }
            y += self
                .interp
                .execute_method(
                    "net/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipComponent",
                    "getHeight",
                    "(Lnet/minecraft/client/gui/Font;)I",
                    &[item.clone(), font.clone()],
                )
                .map(|value| value.as_int())
                .unwrap_or(0);
        }
        y = 0;
        for item in &items {
            let result = self.interp.execute_method(
                "net/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipComponent",
                "extractImage",
                "(Lnet/minecraft/client/gui/Font;IIIILnet/minecraft/client/gui/GuiGraphicsExtractor;)V",
                &[
                    item.clone(),
                    font.clone(),
                    Value::Int(0),
                    Value::Int(y),
                    Value::Int(width),
                    Value::Int(height),
                    Value::Object(Some(graphics)),
                ],
            );
            if let Err(error) = result {
                log::warn!("tooltip extractImage failed: {error}");
            }
            y += self
                .interp
                .execute_method(
                    "net/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipComponent",
                    "getHeight",
                    "(Lnet/minecraft/client/gui/Font;)I",
                    &[item.clone(), font.clone()],
                )
                .map(|value| value.as_int())
                .unwrap_or(0);
        }

        let listeners = self.event_listeners(
            "com/anthonyhilyard/iceberg/events/client/RenderTooltipEvents",
            "POSTEXT",
        );
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
                    Value::Int(0),
                    Value::Int(0),
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

        let draws = self
            .interp
            .heap
            .get_native::<GuiGraphics>(graphics)
            .map(|graphics| graphics.draws.clone())
            .unwrap_or_default();
        Some(crate::bridge::TooltipRender {
            width,
            height,
            draws,
        })
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
        max_damage: i32,
        lines: &[(String, u32)],
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
        let stack = self.interp.heap.alloc_item_stack(
            ItemStack::new(item_id, 1)
                .with_rarity(rarity)
                .with_max_damage(max_damage),
        );
        let graphics = self
            .interp
            .heap
            .alloc_native(GuiGraphics::new(screen_width, screen_height));
        let font = self.minecraft_font();
        let components = self
            .tooltip_component_list(
                item_id,
                rarity,
                max_damage,
                lines,
                screen_width,
                screen_height,
                width.max(0),
            )
            .unwrap_or_else(|| self.interp.heap.alloc_list(Vec::new()));
        self.run_head_injectors(
            "net/minecraft/client/gui/GuiGraphicsExtractor",
            "tooltip",
            "(Lnet/minecraft/client/gui/Font;Ljava/util/List;IILnet/minecraft/client/gui/screens/inventory/tooltip/ClientTooltipPositioner;Lnet/minecraft/resources/Identifier;Z)V",
            Value::Object(Some(graphics)),
            &[
                font.clone(),
                Value::Object(Some(components)),
                Value::Int(x),
                Value::Int(y),
                Value::NULL,
                Value::NULL,
                Value::from_bool(false),
            ],
        );
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
