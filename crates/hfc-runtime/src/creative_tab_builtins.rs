//! Creative-tab placement: `CreativeModeTabEvents.modifyOutputEvent`.
//!
//! Fabric's own bytecode creates the per-tab events (`CreativeModeTabEventsImpl`
//! keeps one `Event` per tab key) and the mod registers its listeners on them.
//! The listeners take a `FabricCreativeModeTabOutput`, which stands for the
//! tab's item list. That list lives on the host, so the runtime replaces the
//! class with a native [`CreativeTabOutput`] that records each call as a
//! [`CreativeTabAddition`] (`FabricBridge::creative_tab_additions`).
//!
//! [`Interpreter::run_creative_tab_events`] runs once the entrypoints are done:
//! every listener of every tab event gets an output for its tab. Calls that
//! need the tab's real contents (`getDisplayStacks`, a predicate anchor, ...) or
//! `MODIFY_OUTPUT_ALL` listeners (they need a `CreativeModeTab`) are logged once
//! and not answered.

use crate::bridge::items::{CreativeTabAddition, TabPlacement};
use crate::builtins::{BuiltinResult, identifier_text, registry_kind};
use crate::heap::Heap;
use crate::interp::Interpreter;
use crate::minecraft::{Item, ItemStack};
use crate::value::{JavaObject, NativeObject, Value};
use std::any::Any;

/// A creative tab a mod registered in code (`Registry.register(..., CREATIVE_MODE_TAB, ...)`).
#[derive(Debug, Clone)]
pub(crate) struct CreativeTabRecord {
    pub id: String,
    pub object: usize,
}

/// A tab's contents as the host loads them: its title, the icon item and the
/// item ids its generator accepted, in order.
#[derive(Debug, Clone, Default)]
pub struct ExportedCreativeTab {
    pub id: String,
    pub title: String,
    pub icon: String,
    pub items: Vec<String>,
}

/// The item list one mod tab's generator writes into, in its own order.
#[derive(Debug, Clone, Default)]
pub(crate) struct CreativeTabItems {
    pub items: Vec<String>,
}

impl NativeObject for CreativeTabItems {
    fn class_name(&self) -> &str {
        "net/fabricmc/fabric/api/creativetab/v1/FabricCreativeModeTabOutput"
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

const OUTPUT: &str = "net/fabricmc/fabric/api/creativetab/v1/FabricCreativeModeTabOutput";
const MODIFY_OUTPUT: &str =
    "net/fabricmc/fabric/api/creativetab/v1/CreativeModeTabEvents$ModifyOutput";
const EVENTS: &str = "net/fabricmc/fabric/api/creativetab/v1/CreativeModeTabEvents";
const EVENTS_IMPL: &str = "net/fabricmc/fabric/impl/creativetab/CreativeModeTabEventsImpl";

/// The item list of one creative tab, as a listener sees it.
#[derive(Debug, Clone)]
pub(crate) struct CreativeTabOutput {
    /// `namespace:path` of the tab.
    pub tab: String,
}

impl NativeObject for CreativeTabOutput {
    fn class_name(&self) -> &str {
        OUTPUT
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// The id of an `ItemLike` or `ItemStack`: a vanilla item, a stack of one, or
/// an item object a mod registered.
fn item_id(heap: &Heap, value: &Value) -> Option<String> {
    let id = value.as_obj()?;
    if let Some(item) = heap.get_native::<Item>(id) {
        return Some(item.id.clone());
    }
    if let Some(stack) = heap.get_native::<ItemStack>(id) {
        return Some(stack.item.id.clone());
    }
    heap.registered_items
        .iter()
        .find(|record| record.object == id)
        .map(|record| record.id.clone())
}

/// The elements of a varargs array or a collection.
fn elements(heap: &Heap, value: &Value) -> Option<Vec<Value>> {
    match heap.get(value.as_obj()?)? {
        JavaObject::Array { elements, .. } => Some(elements.clone()),
        JavaObject::List(items) | JavaObject::Set(items) => Some(items.clone()),
        _ => None,
    }
}

fn note_unsupported(heap: &mut Heap, what: String) {
    if !heap.unsupported_creative_tab_calls.contains(&what) {
        log::warn!("{what} is not modelled; the creative tab additions lack what it does");
        heap.unsupported_creative_tab_calls.push(what);
    }
}

/// The methods of `FabricCreativeModeTabOutput` (and `CreativeModeTab.Output`'s
/// defaults) on a [`CreativeTabOutput`] receiver.
pub(crate) fn output_method(
    heap: &mut Heap,
    method_name: &str,
    descriptor: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    let this = args.first()?.as_obj()?;
    let tab = heap.get_native::<CreativeTabOutput>(this)?.tab.clone();
    let handled = |heap: &mut Heap, ok: bool| {
        if !ok {
            note_unsupported(heap, format!("{OUTPUT}.{method_name}{descriptor}"));
        }
        BuiltinResult::Handled(Value::NULL)
    };
    let add = |heap: &mut Heap, item: String, placement: TabPlacement| {
        heap.creative_tab_additions.push(CreativeTabAddition {
            tab: tab.clone(),
            item,
            placement,
        });
    };

    match method_name {
        "accept" | "prepend" => {
            let Some(item) = args.get(1).and_then(|v| item_id(heap, v)) else {
                return Some(handled(heap, false));
            };
            let placement = if method_name == "accept" {
                TabPlacement::End
            } else {
                TabPlacement::Start
            };
            add(heap, item, placement);
            Some(handled(heap, true))
        }
        "insertAfter" | "insertBefore" => {
            let items = args
                .get(2)
                .and_then(|v| elements(heap, v))
                .map(|values| {
                    values
                        .iter()
                        .map(|v| item_id(heap, v))
                        .collect::<Option<Vec<_>>>()
                })
                .unwrap_or(None);
            let anchor = args.get(1).and_then(|v| item_id(heap, v));
            let (Some(anchor), Some(items)) = (anchor, items) else {
                return Some(handled(heap, false));
            };
            let mut previous = anchor;
            for item in items {
                let placement = if method_name == "insertAfter" {
                    TabPlacement::After(previous.clone())
                } else {
                    TabPlacement::Before(previous.clone())
                };
                if method_name == "insertAfter" {
                    previous = item.clone();
                }
                add(heap, item, placement);
            }
            Some(handled(heap, true))
        }
        "getContext"
        | "getEnabledFeatures"
        | "shouldShowOpRestrictedItems"
        | "getDisplayStacks"
        | "getSearchTabStacks" => Some(handled(heap, false)),
        // A mod's own static lambda also receives the output as its first argument.
        _ => None,
    }
}

/// The text a `Component.literal`/`translatable` carries as its fallback.
fn component_text(heap: &Heap, value: &Value) -> Option<String> {
    let JavaObject::Instance { fields, .. } = heap.get(value.as_obj()?)? else {
        return None;
    };
    heap.get_string(fields.get("text")?.as_obj()?)
        .map(str::to_string)
}

/// `CreativeModeTab.Output` on a tab's own item list, as a mod's generator
/// calls it. `accept` appends; `prepend` starts; `insertAfter`/`insertBefore`
/// put the item next to its anchor when the anchor is in the list.
pub(crate) fn tab_items_method(
    heap: &mut Heap,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    let this = args.first()?.as_obj()?;
    if heap.get_native::<CreativeTabItems>(this).is_none() {
        return None;
    }
    let item = |heap: &Heap, value: &Value| item_id(heap, value);
    let add = |heap: &mut Heap, item: String, after: Option<(&str, bool)>| {
        let Some(list) = heap.get_native_mut::<CreativeTabItems>(this) else {
            return;
        };
        match after {
            None => list.items.push(item),
            Some((anchor, true)) => {
                if let Some(at) = list.items.iter().position(|id| id == anchor) {
                    list.items.insert(at + 1, item);
                } else {
                    list.items.push(item);
                }
            }
            Some((anchor, false)) => {
                if let Some(at) = list.items.iter().position(|id| id == anchor) {
                    list.items.insert(at, item);
                } else {
                    list.items.push(item);
                }
            }
        }
    };
    match method_name {
        "accept" | "add" => {
            let item = item(heap, args.get(1)?)?;
            add(heap, item, None);
            Some(BuiltinResult::Handled(Value::NULL))
        }
        "prepend" => {
            let item = item(heap, args.get(1)?)?;
            if let Some(list) = heap.get_native_mut::<CreativeTabItems>(this) {
                list.items.insert(0, item);
            }
            Some(BuiltinResult::Handled(Value::NULL))
        }
        "insertAfter" | "insertBefore" => {
            let anchor = item(heap, args.get(1)?)?;
            for value in elements(heap, args.get(2)?)? {
                let Some(item) = item(heap, &value) else {
                    continue;
                };
                add(heap, item, Some((&anchor, method_name == "insertAfter")));
            }
            Some(BuiltinResult::Handled(Value::NULL))
        }
        "acceptAll" => {
            for value in elements(heap, args.get(1)?)? {
                if let Some(item) = item(heap, &value) {
                    add(heap, item, None);
                }
            }
            Some(BuiltinResult::Handled(Value::NULL))
        }
        _ => None,
    }
}

impl Interpreter {
    /// `Registry.register(..., CREATIVE_MODE_TAB, ...)` for a tab a mod builds
    /// in code; remembered so its generator can be run after the entrypoints.
    pub(crate) fn register_creative_tab(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[Value],
    ) -> Option<Value> {
        if class_name != "net/minecraft/core/Registry" || method_name != "register" {
            return None;
        }
        let [registry, key, value] = args else {
            return None;
        };
        // The value itself says it is a tab: a mod builds one with
        // `CreativeModeTab.builder()`, whatever shape the registry object has.
        let tab_value = matches!(
            self.heap.get(value.as_obj()?),
            Some(JavaObject::Instance { class_name, .. })
                if class_name.contains("CreativeModeTab")
        );
        if !tab_value && registry_kind(&self.heap, registry).as_deref() != Some("CREATIVE_MODE_TAB")
        {
            return None;
        }
        let id = crate::item_builtins::registration_id(&self.heap, key)?;
        let object = value.as_obj()?;
        if !matches!(self.heap.get(object), Some(JavaObject::Instance { .. })) {
            return Some(value.clone());
        }
        if self
            .heap
            .registered_creative_tabs
            .iter()
            .any(|tab| tab.id == id)
        {
            return Some(value.clone());
        }
        self.heap
            .registered_creative_tabs
            .push(CreativeTabRecord { id, object });
        Some(value.clone())
    }

    /// Run every listener of every `CreativeModeTabEvents.modifyOutputEvent`
    /// event with an output that records the tab placements it asks for, and
    /// then every tab a mod registered in code through its own generator.
    pub(crate) fn run_creative_tab_events(&mut self) {
        let events = self
            .loader
            .get(EVENTS_IMPL)
            .and_then(|class| class.static_fields.get("CREATIVE_MODE_TAB_EVENT_MAP"))
            .and_then(Value::as_obj)
            .and_then(|map| match self.heap.get(map) {
                Some(JavaObject::Map(entries)) => Some(entries.clone()),
                _ => None,
            })
            .unwrap_or_default();
        for (key, event) in events {
            let tab = match self.heap.get(key.as_obj().unwrap_or(usize::MAX)) {
                Some(JavaObject::Instance { fields, .. }) => fields
                    .get("location")
                    .and_then(|location| identifier_text(&self.heap, location)),
                _ => None,
            };
            let Some(tab) = tab else {
                note_unsupported(
                    &mut self.heap,
                    "a tab event keyed by a non-ResourceKey".into(),
                );
                continue;
            };
            let output = self
                .heap
                .alloc_native(CreativeTabOutput { tab: tab.clone() });
            for listener in self.listeners(&event) {
                let result = self.execute_method(
                    MODIFY_OUTPUT,
                    "modifyOutput",
                    &format!("(L{OUTPUT};)V"),
                    &[listener, Value::Object(Some(output))],
                );
                if let Err(e) = result {
                    log::warn!("creative tab listener for {tab} failed: {e}");
                }
            }
        }
        let all = self
            .loader
            .get(EVENTS)
            .and_then(|class| class.static_fields.get("MODIFY_OUTPUT_ALL"))
            .cloned()
            .map(|event| self.listeners(&event).len())
            .unwrap_or(0);
        if all > 0 {
            note_unsupported(
                &mut self.heap,
                "CreativeModeTabEvents.MODIFY_OUTPUT_ALL listeners".into(),
            );
        }
        self.run_registered_creative_tabs();
    }

    /// Run each mod-registered tab's `displayItems` generator into a native
    /// item list and keep the result for the host.
    fn run_registered_creative_tabs(&mut self) {
        let tabs = self.heap.registered_creative_tabs.clone();
        let mut exported = Vec::new();
        for tab in tabs {
            let (title_value, icon_value, generator) = {
                let Some(JavaObject::Instance { fields, .. }) = self.heap.get(tab.object) else {
                    continue;
                };
                (
                    fields.get("displayName").cloned(),
                    fields.get("iconGenerator").cloned(),
                    fields.get("displayItemsGenerator").cloned(),
                )
            };
            let title = title_value
                .as_ref()
                .and_then(|component| component_text(&self.heap, component))
                .unwrap_or_default();
            let icon = icon_value
                .and_then(|generator| {
                    self.execute_method(
                        "java/util/function/Supplier",
                        "get",
                        "()Ljava/lang/Object;",
                        &[generator],
                    )
                    .ok()
                })
                .and_then(|stack| item_id(&self.heap, &stack))
                .unwrap_or_default();
            let items = self.heap.alloc_native(CreativeTabItems::default());
            if let Some(generator) = generator {
                let result = self.execute_method(
                    "net/minecraft/world/item/CreativeModeTab$DisplayItemsGenerator",
                    "accept",
                    "(Lnet/minecraft/world/item/CreativeModeTab$ItemDisplayParameters;Lnet/minecraft/world/item/CreativeModeTab$Output;)V",
                    &[generator, Value::NULL, Value::Object(Some(items))],
                );
                if let Err(error) = result {
                    log::warn!("creative tab {} items failed: {error}", tab.id);
                }
            }
            let items = self
                .heap
                .get_native::<CreativeTabItems>(items)
                .map(|items| items.items.clone())
                .unwrap_or_default();
            // A tab's icon is an item id; when its icon supplier answers an
            // item shape the runtime does not read, its first item stands in.
            let icon = if icon.is_empty() {
                items.first().cloned().unwrap_or_default()
            } else {
                icon
            };
            exported.push(ExportedCreativeTab {
                id: tab.id,
                title,
                icon,
                items,
            });
        }
        self.heap.creative_tab_exports = exported;
    }

    /// The listeners registered on an `Event` object.
    fn listeners(&self, event: &Value) -> Vec<Value> {
        let Some(JavaObject::Instance { fields, .. }) =
            event.as_obj().and_then(|e| self.heap.get(e))
        else {
            return Vec::new();
        };
        fields
            .get("listeners")
            .and_then(|listeners| elements(&self.heap, listeners))
            .unwrap_or_default()
    }
}
