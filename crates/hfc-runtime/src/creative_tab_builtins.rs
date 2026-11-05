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
use crate::builtins::{BuiltinResult, identifier_text};
use crate::heap::Heap;
use crate::interp::Interpreter;
use crate::minecraft::{Item, ItemStack};
use crate::value::{JavaObject, NativeObject, Value};
use std::any::Any;

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

impl Interpreter {
    /// Run every listener of every `CreativeModeTabEvents.modifyOutputEvent`
    /// event with an output that records the tab placements it asks for.
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
