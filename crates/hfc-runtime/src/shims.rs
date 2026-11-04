//! Per-mod shims: stand-ins for classes that belong to one specific mod or its
//! own platform layer.
//!
//! **This file is debt.** The runtime's rule is that a mod exposing a missing
//! general path gets a fix in the shared path (an opcode, a Fabric API event, a
//! Minecraft builtin, a config format, a host boundary), never a branch on a
//! mod's own classes. Everything here predates that rule; it is gathered in one
//! place so it is easy to find and delete, and the call sites in `builtins.rs`
//! and `interp.rs` keep their original order. Do not add another one.
//!
//! * Mod Menu's `HttpUtil` answers every request with a 404, because the host
//!   has no network boundary yet. A general HTTP boundary replaces it.
//! * Right Click Harvest's config (read through JamLib's `ConfigManager`), its
//!   `Config$HungerLevel` / `Config$ExperienceType` values, its platform event
//!   hooks and its `CropBlockAccessor` mixin interface. Running the mod's real
//!   bytecode with a config file/defaults boundary replaces them.

use crate::builtins::BuiltinResult;
use crate::heap::Heap;
use crate::value::{JavaObject, Value};
use std::collections::HashMap;

/// Mod Menu `HttpUtil.request`: no network, so a 404 with an empty JSON body.
pub(crate) fn modmenu_http_util(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
) -> Option<BuiltinResult> {
    if class_name == "com/terraformersmc/modmenu/util/HttpUtil" {
        if method_name == "request" {
            let body_id = heap.alloc_string("{}");
            let mut fields = HashMap::new();
            fields.insert("statusCode".to_string(), Value::Int(404));
            fields.insert("body".to_string(), Value::Object(Some(body_id)));
            let rid = heap.alloc(JavaObject::Instance {
                class_name: "java/net/http/HttpResponse".to_string(),
                fields,
            });
            return Some(BuiltinResult::Handled(Value::Object(Some(rid))));
        }
    }
    None
}

/// JamLib `ConfigManager` as Right Click Harvest uses it: a config object with
/// the mod's defaults, never read from or written to disk.
pub(crate) fn right_click_harvest_config(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    if class_name == "io/github/jamalam360/jamlib/api/config/ConfigManager" {
        if method_name == "<init>" {
            let mut cfg_fields = HashMap::new();
            cfg_fields.insert("requireHoe".into(), Value::from_bool(false));
            cfg_fields.insert("harvestInRadius".into(), Value::from_bool(true));
            let hunger_none = heap.alloc(JavaObject::Instance {
                class_name: "io/github/jamalam360/rightclickharvest/Config$HungerLevel".into(),
                fields: {
                    let mut f = HashMap::new();
                    f.insert("modifier".into(), Value::Float(0.0));
                    f
                },
            });
            cfg_fields.insert("hungerLevel".into(), Value::Object(Some(hunger_none)));
            let exp_none = heap.alloc(JavaObject::Instance {
                class_name: "io/github/jamalam360/rightclickharvest/Config$ExperienceType".into(),
                fields: {
                    let mut f = HashMap::new();
                    f.insert("ordinal".into(), Value::Int(2));
                    f
                },
            });
            cfg_fields.insert("experienceType".into(), Value::Object(Some(exp_none)));
            cfg_fields.insert(
                "hasUserBeenWarnedForNotUsingHoe".into(),
                Value::from_bool(false),
            );
            cfg_fields.insert("showServerWarning".into(), Value::from_bool(false));

            let cfg_id = heap.alloc(JavaObject::Instance {
                class_name: "io/github/jamalam360/rightclickharvest/Config".into(),
                fields: cfg_fields,
            });

            if let Some(this_id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get_mut(this_id) {
                    fields.insert("config".into(), Value::Object(Some(cfg_id)));
                }
            }
            return Some(BuiltinResult::Handled(Value::NULL));
        } else if method_name == "get" {
            if let Some(this_id) = args.get(0).and_then(|v| v.as_obj()) {
                if let Some(JavaObject::Instance { fields, .. }) = heap.get(this_id) {
                    if let Some(cfg) = fields.get("config") {
                        return Some(BuiltinResult::Handled(cfg.clone()));
                    }
                }
            }
        } else if method_name == "save" {
            return Some(BuiltinResult::Handled(Value::NULL));
        }
    }
    None
}

/// Right Click Harvest's `RightClickHarvestPlatform` event hooks: nothing
/// cancels a break or place, and the after-harvest hook is a no-op.
pub(crate) fn right_click_harvest_platform(
    class_name: &str,
    method_name: &str,
) -> Option<BuiltinResult> {
    if class_name == "io/github/jamalam360/rightclickharvest/RightClickHarvestPlatform" {
        if method_name == "postBreakEvent" || method_name == "postPlaceEvent" {
            return Some(BuiltinResult::Handled(Value::from_bool(false)));
        } else if method_name == "postAfterHarvestEvent" {
            return Some(BuiltinResult::Handled(Value::NULL));
        }
    }
    None
}

/// Right Click Harvest's `CropBlockAccessor` mixin interface is answered like
/// `CropBlock` itself.
pub(crate) fn is_crop_block_accessor(class_name: &str) -> bool {
    class_name == "io/github/jamalam360/rightclickharvest/mixin/CropBlockAccessor"
}

/// Static enum-like values of Right Click Harvest's `Config`: the "none" members.
pub(crate) fn right_click_harvest_config_enum(heap: &mut Heap, class_name: &str) -> Option<Value> {
    if class_name == "io/github/jamalam360/rightclickharvest/Config$HungerLevel" {
        let mut f = HashMap::new();
        f.insert("modifier".into(), Value::Float(0.0));
        let id = heap.alloc(JavaObject::Instance {
            class_name: class_name.into(),
            fields: f,
        });
        return Some(Value::Object(Some(id)));
    }

    if class_name == "io/github/jamalam360/rightclickharvest/Config$ExperienceType" {
        let mut f = HashMap::new();
        f.insert("ordinal".into(), Value::Int(2)); // NONE
        let id = heap.alloc(JavaObject::Instance {
            class_name: class_name.into(),
            fields: f,
        });
        return Some(Value::Object(Some(id)));
    }
    None
}
