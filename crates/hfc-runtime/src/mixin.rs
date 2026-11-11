//! Fabric mixin metadata and the injectors used to add widgets to host menus.
//!
//! The runtime is not a general Mixin implementation. It reads the same mixin
//! configs and annotations Fabric Loader uses, and applies only the subset a
//! host menu needs: `@Inject` callbacks that add widgets to a screen's layout.
//! Everything else is recorded as unsupported rather than half-applied.

use crate::class::{AnnotationInfo, AnnotationValue, Class};
use std::fmt::Write as _;

/// Where an injector runs in the target method.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InjectionPoint {
    /// Before the first instruction.
    Head,
    /// Before a return (Mixin's `RETURN`/`TAIL`).
    Tail,
    /// A MixinExtras expression, run at the end of the target method as an
    /// approximation; only menu-layout callbacks that add widgets are used.
    TailApprox,
    /// At a specific method invocation.
    Invoke {
        target_class: String,
        target_method: String,
        target_descriptor: String,
        /// `-1` for every matching invocation.
        ordinal: i32,
        after: bool,
    },
    /// A form this bridge does not apply.
    Unsupported(String),
}

/// How one callback parameter maps to a local of the target method.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalSpec {
    pub type_name: Option<String>,
    pub index: Option<u16>,
    pub ordinal: Option<i32>,
}

/// One `@Inject` callback from a mixin class.
#[derive(Debug, Clone)]
pub struct MixinInjector {
    pub mixin_class: String,
    pub callback_method: String,
    pub callback_descriptor: String,
    pub target_class: String,
    pub target_method: String,
    /// The target descriptor, when the annotation named one.
    pub target_descriptor: Option<String>,
    pub point: InjectionPoint,
    pub cancellable: bool,
    /// One entry per callback parameter after `CallbackInfo`; `None` means a
    /// plain parameter matched by its descriptor type.
    pub locals: Vec<Option<LocalSpec>>,
    /// True when the annotation's call site could not be reproduced exactly and
    /// the callback runs at the end of the target method instead.
    pub approximated: bool,
}

impl MixinInjector {
    pub fn supported(&self) -> bool {
        !matches!(self.point, InjectionPoint::Unsupported(_))
    }
}

/// What an accessor-mixin method does to its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessorKind {
    /// `@Invoker`: calls the target class's method of this name.
    Invoker { method: String },
    /// `@Accessor` getter of a field.
    Getter { field: String },
    /// `@Accessor` setter of a field.
    Setter { field: String },
}

/// One `@Accessor` / `@Invoker` method of a mixin interface. Mixin merges the
/// interface into its target class, so a call on the interface is a call on the
/// target; the interpreter performs that redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixinAccessor {
    /// The mixin interface and the method as callers name it.
    pub interface: String,
    pub method: String,
    /// The accessor method's own descriptor. Mixin generates the bridge with
    /// this descriptor's parameter and return types, so it is also the
    /// descriptor of the target member an invoker calls.
    pub descriptor: String,
    /// The first class the interface is mixed into.
    pub target_class: String,
    /// Every class the interface is mixed into: a mixin may name several
    /// targets, and Mixin generates the bridge on all of them. The interpreter
    /// picks the one the receiver belongs to.
    pub targets: Vec<String>,
    /// Whether the accessor method itself is static. Mixin requires this to
    /// match the target member's static-ness, so it also says whether a call
    /// passes a receiver at all.
    pub is_static: bool,
    pub kind: AccessorKind,
}

/// A method a mixin class adds to its target by implementing an interface
/// (`@Mixin(TextColor.class) class TextColorMixin implements IColor`). A call
/// to the interface method on the target runs the mixin's own bytecode with the
/// target as `this`; its `@Shadow` fields are the target's fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixinInterfaceMethod {
    pub interface: String,
    pub method: String,
    pub descriptor: String,
    pub mixin_class: String,
    pub target_class: String,
}

/// The interface methods a mixin class adds to its targets: its non-static
/// methods with code, other than constructors and Mixin-annotated members
/// (`@Inject`, `@Shadow`, `@Overwrite`, accessors), for each interface it implements.
pub fn interface_methods_from_class(class: &Class) -> Vec<MixinInterfaceMethod> {
    let mut out = Vec::new();
    if class.interfaces.is_empty() {
        return out;
    }
    let Some(targets) = mixin_targets(class) else {
        return out;
    };
    for method in class.methods.values() {
        if method.name.starts_with('<')
            || method.is_static
            || method.code.is_none()
            || method
                .annotations
                .iter()
                .any(|annotation| annotation.type_name.starts_with("org/spongepowered/"))
        {
            continue;
        }
        for interface in &class.interfaces {
            for target in &targets {
                out.push(MixinInterfaceMethod {
                    interface: interface.clone(),
                    method: method.name.clone(),
                    descriptor: method.descriptor.clone(),
                    mixin_class: class.this_class.clone(),
                    target_class: target.clone(),
                });
            }
        }
    }
    out
}

/// Every injector (applied or unsupported) found in the loaded mods.
#[derive(Debug, Clone, Default)]
pub struct MixinManifest {
    pub injectors: Vec<MixinInjector>,
    pub accessors: Vec<MixinAccessor>,
    pub interface_methods: Vec<MixinInterfaceMethod>,
    /// Human-readable notes about annotations this bridge skipped.
    pub unsupported: Vec<String>,
}

/// One entry of a mod's `fabric.mod.json` `mixins` field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixinConfigEntry {
    pub config: String,
    /// `client`, `server` or `*`.
    pub environment: String,
}

/// `fabric.mod.json`'s `mixins` field, in any of its forms.
pub fn config_entries(raw: &str) -> Vec<MixinConfigEntry> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(mixins) = json.get("mixins") else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    let mut add = |value: &serde_json::Value| match value {
        serde_json::Value::String(config) => entries.push(MixinConfigEntry {
            config: config.clone(),
            environment: "client".into(),
        }),
        serde_json::Value::Object(object) => {
            if let Some(config) = object.get("config").and_then(|value| value.as_str()) {
                entries.push(MixinConfigEntry {
                    config: config.to_string(),
                    environment: object
                        .get("environment")
                        .and_then(|value| value.as_str())
                        .unwrap_or("client")
                        .to_string(),
                });
            }
        }
        _ => {}
    };
    match mixins {
        serde_json::Value::Array(values) => values.iter().for_each(&mut add),
        value => add(value),
    }
    entries
}

/// The client mixin class names a mixin config declares (`package` plus its
/// `client` and common `mixins` entries).
pub fn config_classes(config: &str) -> Vec<String> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(config) else {
        return Vec::new();
    };
    let package = json
        .get("package")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim_end_matches('.');
    let mut classes = Vec::new();
    for key in ["mixins", "client"] {
        let Some(entries) = json.get(key).and_then(|value| value.as_array()) else {
            continue;
        };
        for entry in entries {
            let Some(name) = entry.as_str() else {
                continue;
            };
            let full = if package.is_empty() {
                name.to_string()
            } else {
                format!("{package}.{name}")
            };
            classes.push(full.replace('.', "/"));
        }
    }
    classes
}

/// Build the injectors a loaded mixin class declares.
pub fn injectors_from_class(class: &Class) -> Vec<MixinInjector> {
    let mut out = Vec::new();
    let Some(mixin) = class
        .annotations
        .iter()
        .find(|annotation| annotation.type_name == "org/spongepowered/asm/mixin/Mixin")
    else {
        return out;
    };
    let mut targets = Vec::new();
    if let Some(values) = mixin.element("value") {
        collect_class_targets(values, &mut targets);
    }
    if let Some(values) = mixin.element("targets") {
        collect_string_targets(values, &mut targets);
    }
    targets.sort();
    targets.dedup();

    for (_, method) in class.methods.iter() {
        let Some(inject) = method.annotations.iter().find(|annotation| {
            annotation.type_name == "org/spongepowered/asm/mixin/injection/Inject"
        }) else {
            continue;
        };
        let methods = inject.strings("method");
        if methods.is_empty() {
            continue;
        }
        let cancellable = matches!(
            inject.element("cancellable"),
            Some(AnnotationValue::Bool(true))
        );
        let points = injection_points(inject);
        for target_class in &targets {
            for target in &methods {
                let (target_method, target_descriptor) = split_target_method(target);
                if target_method.is_empty() || target_method.contains('*') {
                    continue;
                }
                for point in &points {
                    out.push(MixinInjector {
                        mixin_class: class.this_class.clone(),
                        callback_method: method.name.clone(),
                        callback_descriptor: method.descriptor.clone(),
                        target_class: target_class.clone(),
                        target_method: target_method.to_string(),
                        target_descriptor: target_descriptor.map(str::to_string),
                        point: point.clone(),
                        cancellable,
                        locals: parameter_locals(method),
                        approximated: matches!(point, InjectionPoint::TailApprox),
                    });
                }
            }
        }
    }
    out
}

/// The `@Accessor` / `@Invoker` methods a mixin class declares.
///
/// Mixin generates a bridge for every annotated method: `@Invoker` calls the
/// target class's method, `@Accessor` reads or writes its field. The target
/// member is named by the annotation's `value`, or, when that is absent,
/// inferred from the accessor's own name the way `AccessorInfo.AccessorName`
/// does it: `get`/`is` for a field getter, `set` for a field setter, `call`/
/// `invoke` for a method proxy, each with the prefix removed and the first
/// letter lower-cased (`getHoveredSlot` -> `hoveredSlot`). A field accessor's
/// kind comes from its return type, as `AccessorInfo.initType` decides it:
/// `void` is a setter and everything else a getter.
pub fn accessors_from_class(class: &Class) -> Vec<MixinAccessor> {
    let mut out = Vec::new();
    let Some(targets) = mixin_targets(class) else {
        return out;
    };
    let Some(target_class) = targets.first().cloned() else {
        return out;
    };
    for (_, method) in class.methods.iter() {
        for annotation in &method.annotations {
            // Mixin accepts a full member reference (`name(descriptor)return`)
            // and keeps only the name; the generated bridge uses the
            // accessor's own descriptor either way.
            let explicit = annotation
                .string("value")
                .filter(|name| !name.is_empty())
                .map(accessor_member_name)
                .filter(|name| !name.is_empty());
            let kind = match annotation.type_name.as_str() {
                "org/spongepowered/asm/mixin/gen/Invoker" => {
                    let name = explicit
                        .map(str::to_string)
                        .unwrap_or_else(|| accessor_target_name(&method.name, &["invoke", "call"]));
                    AccessorKind::Invoker { method: name }
                }
                "org/spongepowered/asm/mixin/gen/Accessor" => {
                    let setter = method.descriptor.ends_with(")V");
                    let name = explicit.map(str::to_string).unwrap_or_else(|| {
                        let prefixes: &[&str] = if setter { &["set"] } else { &["get", "is"] };
                        accessor_target_name(&method.name, prefixes)
                    });
                    if setter {
                        AccessorKind::Setter { field: name }
                    } else {
                        AccessorKind::Getter { field: name }
                    }
                }
                _ => continue,
            };
            out.push(MixinAccessor {
                interface: class.this_class.clone(),
                method: method.name.clone(),
                descriptor: method.descriptor.clone(),
                target_class: target_class.clone(),
                targets: targets.clone(),
                is_static: method.is_static,
                kind,
            });
        }
    }
    out
}

/// The name of a Mixin member reference: `name`, `name(descriptor)return` and a
/// lone `(descriptor)return` all yield the name part (empty for the last).
fn accessor_member_name(value: &str) -> &str {
    match value.find('(') {
        Some(index) => &value[..index],
        None => value,
    }
}

/// Mixin's rule for an unnamed accessor: drop the `get`/`set`/`invoke` prefix and
/// lower-case the first letter.
fn accessor_target_name(method: &str, prefixes: &[&str]) -> String {
    for prefix in prefixes {
        if let Some(rest) = method.strip_prefix(prefix) {
            let mut chars = rest.chars();
            if let Some(first) = chars.next() {
                if first.is_uppercase() {
                    return first.to_lowercase().chain(chars).collect();
                }
            }
        }
    }
    method.to_string()
}

fn mixin_targets(class: &Class) -> Option<Vec<String>> {
    let mixin = class
        .annotations
        .iter()
        .find(|annotation| annotation.type_name == "org/spongepowered/asm/mixin/Mixin")?;
    let mut targets = Vec::new();
    if let Some(values) = mixin.element("value") {
        collect_class_targets(values, &mut targets);
    }
    if let Some(values) = mixin.element("targets") {
        collect_string_targets(values, &mut targets);
    }
    Some(targets)
}

fn injection_points(inject: &AnnotationInfo) -> Vec<InjectionPoint> {
    let ats = inject.annotations("at");
    if ats.is_empty() {
        return vec![InjectionPoint::Unsupported("missing @At".into())];
    }
    let mut points = Vec::new();
    for at in ats {
        let value = at.string("value").unwrap_or("");
        let point = match value {
            "HEAD" => InjectionPoint::Head,
            "RETURN" | "TAIL" => InjectionPoint::Tail,
            "INVOKE" => match at.string("target").and_then(parse_member_ref) {
                Some((target_class, target_method, target_descriptor)) => {
                    let ordinal = match at.element("ordinal") {
                        Some(AnnotationValue::Int(ordinal)) => *ordinal,
                        _ => -1,
                    };
                    let after = matches!(
                        at.element("shift"),
                        Some(AnnotationValue::Enum { name, .. }) if name == "AFTER"
                    );
                    InjectionPoint::Invoke {
                        target_class,
                        target_method,
                        target_descriptor,
                        ordinal,
                        after,
                    }
                }
                None => {
                    InjectionPoint::Unsupported(format!("INVOKE target {:?}", at.string("target")))
                }
            },
            // MixinExtras expressions are resolved by Mixin at runtime. A
            // menu callback that adds a widget to a captured layout can still
            // run at the end of the layout method; mark it approximated.
            value if value.starts_with("MIXINEXTRAS:") => InjectionPoint::TailApprox,
            other => InjectionPoint::Unsupported(format!("@At({other})")),
        };
        points.push(point);
    }
    points
}

fn collect_class_targets(value: &AnnotationValue, out: &mut Vec<String>) {
    match value {
        AnnotationValue::Class(name) => out.push(name.clone()),
        AnnotationValue::Array(values) => {
            for value in values {
                collect_class_targets(value, out);
            }
        }
        _ => {}
    }
}

fn collect_string_targets(value: &AnnotationValue, out: &mut Vec<String>) {
    match value {
        AnnotationValue::String(name) => out.push(name.replace('.', "/")),
        AnnotationValue::Array(values) => {
            for value in values {
                collect_string_targets(value, out);
            }
        }
        _ => {}
    }
}

/// `name` or `name(descriptor)return`.
fn split_target_method(value: &str) -> (&str, Option<&str>) {
    match value.find('(') {
        Some(index) => (&value[..index], Some(&value[index..])),
        None => (value, None),
    }
}

/// A Mixin `@At(target = ...)` member reference:
/// `Lpkg/Class;method(desc)ret` or `pkg.Class.method(desc)ret`.
fn parse_member_ref(value: &str) -> Option<(String, String, String)> {
    let value = value.trim();
    let (class, rest) = if let Some(stripped) = value.strip_prefix('L') {
        let end = stripped.find(';')?;
        (stripped[..end].to_string(), &stripped[end + 1..])
    } else {
        let paren = value.find('(')?;
        let dot = value[..paren].rfind('.')?;
        (value[..dot].replace('.', "/"), &value[dot + 1..])
    };
    let paren = rest.find('(')?;
    Some((class, rest[..paren].to_string(), rest[paren..].to_string()))
}

/// The parameter types of a JVM method descriptor.
pub fn descriptor_parameter_types(descriptor: &str) -> Vec<String> {
    fn parse_type(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
        let Some(c) = chars.next() else {
            return None;
        };
        let mut ty = String::new();
        match c {
            'L' => {
                ty.push('L');
                for next in chars.by_ref() {
                    ty.push(next);
                    if next == ';' {
                        return Some(ty);
                    }
                }
                None
            }
            '[' => {
                ty.push('[');
                ty.push_str(&parse_type(chars)?);
                Some(ty)
            }
            other => {
                ty.push(other);
                Some(ty)
            }
        }
    }

    let Some(start) = descriptor.find('(') else {
        return Vec::new();
    };
    let Some(end) = descriptor.rfind(')') else {
        return Vec::new();
    };
    let mut chars = descriptor[start + 1..end].chars().peekable();
    let mut out = Vec::new();
    while let Some(ty) = parse_type(&mut chars) {
        out.push(ty);
    }
    out
}

fn parameter_locals(method: &crate::class::MethodInfo) -> Vec<Option<LocalSpec>> {
    let types = descriptor_parameter_types(&method.descriptor);
    let mut out = Vec::new();
    // Skip the CallbackInfo parameter (always first).
    for index in 1..types.len() {
        let annotations = method
            .parameter_annotations
            .get(index)
            .cloned()
            .unwrap_or_default();
        let local = annotations.iter().find(|annotation| {
            annotation.type_name.ends_with("/Local")
                || annotation.type_name == "org/spongepowered/asm/mixin/injection/Local"
        });
        out.push(local.map(|annotation| LocalSpec {
            type_name: annotation.element("type").and_then(|value| match value {
                AnnotationValue::Class(name) => Some(name.clone()),
                _ => None,
            }),
            index: match annotation.element("index") {
                Some(AnnotationValue::Int(index)) => Some(*index as u16),
                _ => None,
            },
            ordinal: match annotation.element("ordinal") {
                Some(AnnotationValue::Int(ordinal)) => Some(*ordinal),
                _ => None,
            },
        }));
    }
    out
}

/// Render an annotation for diagnostics.
pub fn describe_annotation(annotation: &AnnotationInfo) -> String {
    let mut out = String::new();
    let _ = write!(out, "@{}", annotation.type_name);
    for (name, value) in &annotation.elements {
        let rendered = match value {
            AnnotationValue::String(value) => format!("{value:?}"),
            AnnotationValue::Int(value) => value.to_string(),
            AnnotationValue::Bool(value) => value.to_string(),
            AnnotationValue::Enum { name, .. } => name.clone(),
            AnnotationValue::Class(name) => name.clone(),
            AnnotationValue::Array(values) => format!("{} values", values.len()),
            _ => "?".into(),
        };
        let _ = write!(out, " {name}={rendered}");
    }
    out
}

/// Read one entry out of a zip archive.
pub(crate) fn read_zip_entry<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut resource = archive.by_name(name).ok()?;
    let mut bytes = Vec::new();
    resource.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// Read one resource out of a mod jar (or a nested jar inside it).
pub(crate) fn read_mod_resource(jar: &str, nested: Option<&str>, entry: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(jar).ok()?;
    let mut archive = zip::ZipArchive::new(file).ok()?;
    match nested {
        None => read_zip_entry(&mut archive, entry),
        Some(nested) => {
            let nested_bytes = read_zip_entry(&mut archive, nested)?;
            let mut inner = zip::ZipArchive::new(std::io::Cursor::new(nested_bytes)).ok()?;
            read_zip_entry(&mut inner, entry)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_mixin_config_forms() {
        let entries = config_entries(
            r#"{"mixins":["a.mixins.json",{"config":"b.mixins.json","environment":"server"}]}"#,
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].config, "a.mixins.json");
        assert_eq!(entries[1].environment, "server");
    }

    #[test]
    fn resolves_config_classes_under_the_package() {
        let classes = config_classes(
            r#"{"package":"com.example.mixin","mixins":["Common"],"client":["Client"],"server":["Server"]}"#,
        );
        assert_eq!(
            classes,
            vec![
                "com/example/mixin/Common".to_string(),
                "com/example/mixin/Client".to_string()
            ]
        );
    }

    #[test]
    fn parses_invoke_member_references() {
        assert_eq!(
            parse_member_ref(
                "Lnet/minecraft/client/gui/layouts/GridLayout$RowHelper;addChild(Lnet/minecraft/client/gui/layouts/LayoutElement;I)Lnet/minecraft/client/gui/layouts/LayoutElement;"
            ),
            Some((
                "net/minecraft/client/gui/layouts/GridLayout$RowHelper".to_string(),
                "addChild".to_string(),
                "(Lnet/minecraft/client/gui/layouts/LayoutElement;I)Lnet/minecraft/client/gui/layouts/LayoutElement;".to_string()
            ))
        );
        assert_eq!(
            parse_member_ref("net.minecraft.client.gui.screens.TitleScreen.init()V"),
            Some((
                "net/minecraft/client/gui/screens/TitleScreen".to_string(),
                "init".to_string(),
                "()V".to_string()
            ))
        );
    }

    #[test]
    fn splits_descriptor_parameters() {
        assert_eq!(
            descriptor_parameter_types(
                "(Lnet/minecraft/client/gui/layouts/LinearLayout;I[Ljava/lang/String;Z)V"
            ),
            vec![
                "Lnet/minecraft/client/gui/layouts/LinearLayout;".to_string(),
                "I".to_string(),
                "[Ljava/lang/String;".to_string(),
                "Z".to_string()
            ]
        );
    }
}
