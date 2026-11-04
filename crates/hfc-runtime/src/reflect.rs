//! Reflection over the classes the interpreter has loaded: `Class.getFields`,
//! `java.lang.reflect.Field` and annotation instances.
//!
//! Libraries that serialize a plain object to a config file (Jankson, Gson,
//! Night Config's object converter) walk its fields through reflection. The
//! class metadata lives in the loader, which builtins cannot see, so these
//! calls are answered here, ahead of the builtins.

use crate::class::{AnnotationInfo, AnnotationValue, FieldInfo, MethodInfo};
use crate::interp::Interpreter;
use crate::value::{JavaObject, Value};
use std::collections::HashMap;

const ACC_PUBLIC: u16 = 0x0001;
const ACC_ENUM: u16 = 0x4000;

/// The class object name for a field descriptor (`Z` is `boolean`).
fn type_name(descriptor: &str) -> String {
    match descriptor {
        "Z" => "boolean".into(),
        "B" => "byte".into(),
        "C" => "char".into(),
        "S" => "short".into(),
        "I" => "int".into(),
        "J" => "long".into(),
        "F" => "float".into(),
        "D" => "double".into(),
        d if d.starts_with('L') && d.ends_with(';') => d[1..d.len() - 1].to_string(),
        d => d.to_string(),
    }
}

/// The top-level type arguments of a generic signature
/// (`Lfoo/Bar<Lbaz/Qux;Ljava/util/List<TT;>;>;` gives `baz/Qux`, `java/util/List`).
fn signature_type_arguments(signature: &str) -> Vec<String> {
    let Some(open) = signature.find('<') else {
        return Vec::new();
    };
    let chars: Vec<char> = signature[open + 1..].chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() && chars[i] != '>' {
        // Wildcard markers and the unbounded wildcard.
        if matches!(chars[i], '+' | '-') {
            i += 1;
        }
        match chars.get(i) {
            Some('*') => {
                out.push("java/lang/Object".to_string());
                i += 1;
            }
            Some('L') => {
                let mut depth = 0;
                let mut name = String::new();
                let mut in_name = true;
                i += 1;
                while i < chars.len() {
                    match chars[i] {
                        '<' => {
                            depth += 1;
                            in_name = false;
                        }
                        '>' => depth -= 1,
                        ';' if depth == 0 => {
                            i += 1;
                            break;
                        }
                        c if in_name => name.push(c),
                        _ => {}
                    }
                    i += 1;
                }
                out.push(name);
            }
            // A type variable or an array: not a class this can name.
            Some('T') => {
                while i < chars.len() && chars[i] != ';' {
                    i += 1;
                }
                i += 1;
                out.push("java/lang/Object".to_string());
            }
            _ => i += 1,
        }
    }
    out
}

/// The parameter types of a method descriptor, as class object names.
fn parameter_types(descriptor: &str) -> Vec<String> {
    let inner = descriptor
        .strip_prefix('(')
        .and_then(|d| d.split(')').next())
        .unwrap_or("");
    let mut out = Vec::new();
    let bytes: Vec<char> = inner.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        while bytes[i] == '[' {
            i += 1;
        }
        if bytes[i] == 'L' {
            while bytes[i] != ';' {
                i += 1;
            }
        }
        i += 1;
        let one: String = bytes[start..i].iter().collect();
        out.push(if one.starts_with('[') {
            one
        } else {
            type_name(&one)
        });
    }
    out
}

fn return_type(descriptor: &str) -> String {
    let ret = descriptor.rsplit(')').next().unwrap_or("V");
    if ret == "V" {
        "void".into()
    } else {
        type_name(ret)
    }
}

fn default_value(descriptor: &str) -> Value {
    match descriptor {
        "J" => Value::Long(0),
        "F" => Value::Float(0.0),
        "D" => Value::Double(0.0),
        "Z" | "B" | "C" | "S" | "I" => Value::Int(0),
        _ => Value::NULL,
    }
}

impl Interpreter {
    fn class_arg(&self, value: &Value) -> Option<String> {
        match self.heap.get(value.as_obj()?)? {
            JavaObject::Class(name) => Some(name.replace('.', "/")),
            _ => None,
        }
    }

    fn string_field(&self, id: usize, name: &str) -> Option<String> {
        match self.heap.get(id)? {
            JavaObject::Instance { fields, .. } => {
                let sid = fields.get(name)?.as_obj()?;
                self.heap.get_string(sid).map(str::to_string)
            }
            _ => None,
        }
    }

    /// `(declaring class, field)` of a `Field` object.
    fn field_of(&mut self, field_obj: &Value) -> Option<(String, FieldInfo)> {
        let id = field_obj.as_obj()?;
        let owner = self.string_field(id, "declaringClass")?;
        let name = self.string_field(id, "name")?;
        let class = self.loader.get_or_load_mut(&owner).ok()??;
        Some((owner, class.fields.get(&name)?.clone()))
    }

    /// The fields a class declares, in declaration order.
    fn declared_fields(&mut self, class: &str) -> Vec<(String, FieldInfo)> {
        let Ok(Some(c)) = self.loader.get_or_load_mut(class) else {
            return Vec::new();
        };
        let mut fields: Vec<FieldInfo> = c.fields.values().cloned().collect();
        fields.sort_by_key(|f| f.index);
        fields.into_iter().map(|f| (class.to_string(), f)).collect()
    }

    fn field_objects(&mut self, class: &str, public_only: bool) -> Value {
        let mut found = Vec::new();
        let mut current = Some(class.to_string());
        while let Some(name) = current {
            found.extend(
                self.declared_fields(&name)
                    .into_iter()
                    .filter(|(_, f)| !public_only || f.access & ACC_PUBLIC != 0),
            );
            if !public_only {
                break;
            }
            current = self
                .loader
                .get_or_load_mut(&name)
                .ok()
                .flatten()
                .and_then(|c| c.super_class.clone())
                .filter(|s| s != "java/lang/Object");
        }
        let mut elements = Vec::new();
        for (owner, info) in found {
            let owner_id = self.heap.alloc_string(owner.clone());
            let name_id = self.heap.alloc_string(info.name.clone());
            let mut fields = HashMap::new();
            fields.insert("declaringClass".to_string(), Value::Object(Some(owner_id)));
            fields.insert("name".to_string(), Value::Object(Some(name_id)));
            let id = self.heap.alloc(JavaObject::Instance {
                class_name: "java/lang/reflect/Field".into(),
                fields,
            });
            elements.push(Value::Object(Some(id)));
        }
        let aid = self.heap.alloc(JavaObject::Array {
            elem_type: "java/lang/reflect/Field".into(),
            elements,
        });
        Value::Object(Some(aid))
    }

    /// A boxed value for a field of descriptor `descriptor`. A boolean is a
    /// `Boolean` instance, since a plain int could not be told from an `Integer`.
    fn box_field_value(&mut self, descriptor: &str, value: Value) -> Value {
        if descriptor != "Z" {
            return value;
        }
        let mut fields = HashMap::new();
        fields.insert("value".to_string(), Value::from_bool(value.as_bool()));
        let id = self.heap.alloc(JavaObject::Instance {
            class_name: "java/lang/Boolean".into(),
            fields,
        });
        Value::Object(Some(id))
    }

    fn unbox_field_value(&self, descriptor: &str, value: &Value) -> Value {
        if descriptor == "Z" {
            if let Some(JavaObject::Instance { fields, .. }) =
                value.as_obj().and_then(|id| self.heap.get(id))
            {
                if let Some(inner) = fields.get("value") {
                    return Value::from_bool(inner.as_bool());
                }
            }
        }
        value.clone()
    }

    fn annotation_object(&mut self, annotation: &AnnotationInfo) -> Value {
        let mut fields = HashMap::new();
        fields.insert("$annotation".to_string(), Value::Int(1));
        for (name, value) in &annotation.elements {
            let value = self.annotation_value(value);
            fields.insert(name.clone(), value);
        }
        let id = self.heap.alloc(JavaObject::Instance {
            class_name: annotation.type_name.clone(),
            fields,
        });
        Value::Object(Some(id))
    }

    fn annotation_value(&mut self, value: &AnnotationValue) -> Value {
        match value {
            AnnotationValue::Int(v) => Value::Int(*v),
            AnnotationValue::Long(v) => Value::Long(*v),
            AnnotationValue::Float(v) => Value::Float(*v),
            AnnotationValue::Double(v) => Value::Double(*v),
            AnnotationValue::Bool(v) => Value::from_bool(*v),
            AnnotationValue::Char(v) => Value::Int(*v as i32),
            AnnotationValue::String(v) => Value::Object(Some(self.heap.alloc_string(v.clone()))),
            AnnotationValue::Class(v) => Value::Object(Some(self.heap.alloc_class(v.clone()))),
            AnnotationValue::Annotation(v) => self.annotation_object(v),
            AnnotationValue::Array(values) => {
                let elements = values.iter().map(|v| self.annotation_value(v)).collect();
                Value::Object(Some(self.heap.alloc(JavaObject::Array {
                    elem_type: "java/lang/Object".into(),
                    elements,
                })))
            }
            AnnotationValue::Enum { .. } => Value::NULL,
        }
    }

    /// Answer a reflection call, or `None` when the call is not reflection
    /// over loaded classes and normal dispatch should go on.
    pub(crate) fn reflect_call(
        &mut self,
        class_name: &str,
        method_name: &str,
        args: &[Value],
    ) -> Option<Value> {
        let receiver = args.first()?;
        match class_name {
            // `Class.forName(name)` initializes the class, which is how a
            // library registers itself (Night Config's format detector loads
            // `TomlFormat` for its static registration).
            "java/lang/Class" if method_name == "forName" => {
                let name = self.heap.get_string(receiver.as_obj()?)?.replace('.', "/");
                if !self.loader.has_class(&name) {
                    return None;
                }
                let _ = self.ensure_initialized(&name);
                Some(Value::Object(Some(self.heap.alloc_class(name))))
            }
            "java/lang/Class" => {
                let class = self.class_arg(receiver)?;
                match method_name {
                    "getFields" => Some(self.field_objects(&class, true)),
                    "getDeclaredFields" => Some(self.field_objects(&class, false)),
                    "getSuperclass" => {
                        let class = self.loader.get_or_load_mut(&class).ok()??;
                        Some(match class.super_class.clone() {
                            Some(parent) if !class.is_interface => {
                                Value::Object(Some(self.heap.alloc_class(parent)))
                            }
                            _ => Value::NULL,
                        })
                    }
                    "getInterfaces" => {
                        let names = self
                            .loader
                            .get_or_load_mut(&class)
                            .ok()??
                            .interfaces
                            .clone();
                        let classes = names
                            .into_iter()
                            .map(|n| Value::Object(Some(self.heap.alloc_class(n))))
                            .collect();
                        Some(self.array_of("java/lang/Class", classes))
                    }
                    "isInterface" => Some(Value::from_bool(
                        self.loader.get_or_load_mut(&class).ok()??.is_interface,
                    )),
                    "isEnum" => {
                        let c = self.loader.get_or_load_mut(&class).ok()??;
                        Some(Value::from_bool(
                            c.super_class.as_deref() == Some("java/lang/Enum"),
                        ))
                    }
                    "getDeclaredMethods" => {
                        let methods = self.method_objects(&class, false);
                        Some(self.array_of("java/lang/reflect/Method", methods))
                    }
                    "getMethods" => {
                        let methods = self.method_objects(&class, true);
                        Some(self.array_of("java/lang/reflect/Method", methods))
                    }
                    "getDeclaredMethod" | "getMethod" if args.len() >= 2 => {
                        let found = self.find_method(&class, args, method_name == "getMethod");
                        // The real call throws NoSuchMethodException; callers
                        // that catch it fall back, a missing method is null.
                        Some(found.unwrap_or(Value::NULL))
                    }
                    "getField" | "getDeclaredField" if args.len() == 2 => {
                        let wanted = self.heap.get_string(args[1].as_obj()?)?.to_string();
                        let all = self.field_objects(&class, method_name == "getField");
                        let JavaObject::Array { elements, .. } = self.heap.get(all.as_obj()?)?
                        else {
                            return None;
                        };
                        let elements = elements.clone();
                        elements.into_iter().find(|field| {
                            field
                                .as_obj()
                                .and_then(|id| self.string_field(id, "name"))
                                .is_some_and(|name| name == wanted)
                        })
                    }
                    "getEnumConstants" => {
                        let _ = self.ensure_initialized(&class);
                        let constants: Vec<String> = self
                            .declared_fields(&class)
                            .into_iter()
                            .filter(|(_, f)| f.access & ACC_ENUM != 0)
                            .map(|(_, f)| f.name)
                            .collect();
                        if constants.is_empty() {
                            return Some(Value::NULL);
                        }
                        let statics = self.loader.get(&class)?.static_fields.clone();
                        let elements = constants
                            .iter()
                            .map(|name| statics.get(name).cloned().unwrap_or(Value::NULL))
                            .collect();
                        Some(Value::Object(Some(self.heap.alloc(JavaObject::Array {
                            elem_type: class.clone(),
                            elements,
                        }))))
                    }
                    "isAssignableFrom" if args.len() == 2 => {
                        let other = self.class_arg(&args[1])?;
                        Some(Value::from_bool(
                            self.class_is_assignable_to(&other, &class),
                        ))
                    }
                    "isInstance" if args.len() == 2 => {
                        if args[1].is_null() {
                            return Some(Value::from_bool(false));
                        }
                        Some(Value::from_bool(self.is_instance_of(&args[1], &class)))
                    }
                    _ => None,
                }
            }
            "java/lang/reflect/Method"
                if !matches!(
                    method_name,
                    "setAccessible" | "trySetAccessible" | "canAccess" | "isAccessible"
                ) =>
            {
                self.reflect_method_call(method_name, args)
            }
            "java/lang/reflect/Method" | "java/lang/reflect/Constructor"
                if matches!(
                    method_name,
                    "setAccessible" | "trySetAccessible" | "canAccess" | "isAccessible"
                ) =>
            {
                Some(match method_name {
                    "setAccessible" => Value::NULL,
                    _ => Value::from_bool(true),
                })
            }
            "java/util/ServiceLoader" => self.service_loader_call(method_name, args),
            "java/lang/reflect/Field" | "java/lang/reflect/AccessibleObject" => {
                self.reflect_field_call(method_name, args)
            }
            "java/lang/reflect/ParameterizedType" => {
                let id = receiver.as_obj()?;
                let JavaObject::Instance { fields, .. } = self.heap.get(id)? else {
                    return None;
                };
                match method_name {
                    "getActualTypeArguments" => fields.get("arguments").cloned(),
                    "getRawType" => fields.get("raw").cloned(),
                    _ => None,
                }
            }
            _ => {
                // An annotation instance answers its element methods.
                let id = receiver.as_obj()?;
                match self.heap.get(id)? {
                    JavaObject::Instance { fields, .. } if fields.contains_key("$annotation") => {
                        match method_name {
                            "annotationType" => None,
                            name => fields.get(name).cloned(),
                        }
                    }
                    _ => None,
                }
            }
        }
    }

    /// The providers a `META-INF/services/<service>` file names, built with
    /// their no-argument constructors.
    fn service_providers(&mut self, service: &str) -> Vec<Value> {
        let path = format!("META-INF/services/{}", service.replace('/', "."));
        let mut names = Vec::new();
        for text in self.loader.text_resources(&path) {
            for line in text.lines() {
                let name = line.split('#').next().unwrap_or("").trim();
                if !name.is_empty() && !names.contains(&name.to_string()) {
                    names.push(name.to_string());
                }
            }
        }
        let mut providers = Vec::new();
        for name in names {
            let class = name.replace('.', "/");
            if self.ensure_initialized(&class).is_err() {
                continue;
            }
            let id = self.alloc_uninitialized(&class);
            let this = Value::Object(Some(id));
            if self
                .execute_method(&class, "<init>", "()V", std::slice::from_ref(&this))
                .is_ok()
            {
                providers.push(this);
            }
        }
        providers
    }

    fn service_loader_call(&mut self, method_name: &str, args: &[Value]) -> Option<Value> {
        match method_name {
            "load" | "loadInstalled" => {
                let service = self.class_arg(args.first()?)?;
                let sid = self.heap.alloc_string(service);
                let mut fields = HashMap::new();
                fields.insert("serviceClass".to_string(), Value::Object(Some(sid)));
                Some(Value::Object(Some(self.heap.alloc(JavaObject::Instance {
                    class_name: "java/util/ServiceLoader".into(),
                    fields,
                }))))
            }
            "findFirst" | "iterator" | "stream" => {
                let service = self.string_field(args.first()?.as_obj()?, "serviceClass")?;
                let providers = self.service_providers(&service);
                match method_name {
                    "findFirst" => {
                        let mut fields = HashMap::new();
                        if let Some(first) = providers.into_iter().next() {
                            fields.insert("value".to_string(), first);
                        }
                        Some(Value::Object(Some(self.heap.alloc(JavaObject::Instance {
                            class_name: "java/util/Optional".into(),
                            fields,
                        }))))
                    }
                    "iterator" => {
                        let list = self.heap.alloc_list(providers);
                        self.execute_method(
                            "java/util/List",
                            "iterator",
                            "()Ljava/util/Iterator;",
                            &[Value::Object(Some(list))],
                        )
                        .ok()
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn declared_methods(&mut self, class: &str) -> Vec<(String, MethodInfo)> {
        let Ok(Some(c)) = self.loader.get_or_load_mut(class) else {
            return Vec::new();
        };
        let mut methods: Vec<MethodInfo> = c
            .methods
            .values()
            .filter(|m| !m.name.starts_with('<'))
            .cloned()
            .collect();
        methods.sort_by(|a, b| (&a.name, &a.descriptor).cmp(&(&b.name, &b.descriptor)));
        methods
            .into_iter()
            .map(|m| (class.to_string(), m))
            .collect()
    }

    fn method_objects(&mut self, class: &str, public_only: bool) -> Vec<Value> {
        let mut found = Vec::new();
        let mut current = Some(class.to_string());
        while let Some(name) = current {
            found.extend(
                self.declared_methods(&name)
                    .into_iter()
                    .filter(|(_, m)| !public_only || m.access & ACC_PUBLIC != 0),
            );
            if !public_only {
                break;
            }
            current = self
                .loader
                .get_or_load_mut(&name)
                .ok()
                .flatten()
                .and_then(|c| c.super_class.clone())
                .filter(|s| s != "java/lang/Object");
        }
        found
            .into_iter()
            .map(|(owner, info)| {
                let mut fields = HashMap::new();
                for (key, text) in [
                    ("declaringClass", owner),
                    ("name", info.name),
                    ("descriptor", info.descriptor),
                ] {
                    let id = self.heap.alloc_string(text);
                    fields.insert(key.to_string(), Value::Object(Some(id)));
                }
                Value::Object(Some(self.heap.alloc(JavaObject::Instance {
                    class_name: "java/lang/reflect/Method".into(),
                    fields,
                })))
            })
            .collect()
    }

    fn array_of(&mut self, elem_type: &str, elements: Vec<Value>) -> Value {
        Value::Object(Some(self.heap.alloc(JavaObject::Array {
            elem_type: elem_type.into(),
            elements,
        })))
    }

    fn find_method(&mut self, class: &str, args: &[Value], public_only: bool) -> Option<Value> {
        let wanted = self.heap.get_string(args.get(1)?.as_obj()?)?.to_string();
        let params: Vec<String> = match args.get(2).and_then(|v| v.as_obj()) {
            Some(id) => match self.heap.get(id)? {
                JavaObject::Array { elements, .. } => elements
                    .clone()
                    .iter()
                    .map(|c| self.class_arg(c).unwrap_or_default())
                    .collect(),
                _ => Vec::new(),
            },
            None => Vec::new(),
        };
        self.method_objects(class, public_only)
            .into_iter()
            .find(|m| {
                let Some(id) = m.as_obj() else { return false };
                self.string_field(id, "name").as_deref() == Some(wanted.as_str())
                    && self
                        .string_field(id, "descriptor")
                        .is_some_and(|d| parameter_types(&d) == params)
            })
    }

    fn method_of(&mut self, method_obj: &Value) -> Option<(String, MethodInfo)> {
        let id = method_obj.as_obj()?;
        let owner = self.string_field(id, "declaringClass")?;
        let name = self.string_field(id, "name")?;
        let descriptor = self.string_field(id, "descriptor")?;
        let class = self.loader.get_or_load_mut(&owner).ok()??;
        Some((owner, class.methods.get(&(name, descriptor))?.clone()))
    }

    fn reflect_method_call(&mut self, method_name: &str, args: &[Value]) -> Option<Value> {
        let (owner, info) = self.method_of(&args[0])?;
        match method_name {
            "getName" => Some(Value::Object(Some(self.heap.alloc_string(info.name)))),
            "getModifiers" => Some(Value::Int(info.access as i32)),
            "getDeclaringClass" => Some(Value::Object(Some(self.heap.alloc_class(owner)))),
            "getReturnType" | "getGenericReturnType" => Some(Value::Object(Some(
                self.heap.alloc_class(return_type(&info.descriptor)),
            ))),
            "getParameterCount" => Some(Value::Int(parameter_types(&info.descriptor).len() as i32)),
            "getParameterTypes" | "getGenericParameterTypes" => {
                let classes = parameter_types(&info.descriptor)
                    .into_iter()
                    .map(|t| Value::Object(Some(self.heap.alloc_class(t))))
                    .collect();
                Some(self.array_of("java/lang/Class", classes))
            }
            "isSynthetic" | "isBridge" | "isDefault" | "isVarArgs" => Some(Value::from_bool(false)),
            "getAnnotation" | "getDeclaredAnnotation" | "isAnnotationPresent"
                if args.len() == 2 =>
            {
                let wanted = self.class_arg(&args[1])?;
                let found = info.annotations.iter().find(|a| a.type_name == wanted);
                if method_name == "isAnnotationPresent" {
                    return Some(Value::from_bool(found.is_some()));
                }
                match found.cloned() {
                    Some(annotation) => Some(self.annotation_object(&annotation)),
                    None => Some(Value::NULL),
                }
            }
            "invoke" if args.len() >= 2 => {
                let given: Vec<Value> = match args.get(2).and_then(|v| v.as_obj()) {
                    Some(id) => match self.heap.get(id)? {
                        JavaObject::Array { elements, .. } => elements.clone(),
                        _ => Vec::new(),
                    },
                    None => Vec::new(),
                };
                let param_descriptors = parameter_types(&info.descriptor);
                let mut call_args = Vec::new();
                if !info.is_static {
                    call_args.push(args[1].clone());
                }
                for (value, ty) in given.iter().zip(&param_descriptors) {
                    call_args.push(if ty == "boolean" {
                        self.unbox_field_value("Z", value)
                    } else {
                        value.clone()
                    });
                }
                let result = if info.is_static {
                    self.ensure_initialized(&owner).ok()?;
                    self.execute_special_method(&owner, &info.name, &info.descriptor, &call_args)
                } else {
                    self.execute_method(&owner, &info.name, &info.descriptor, &call_args)
                };
                let value = match result {
                    Ok(value) => value,
                    Err(error) => {
                        log::warn!("reflective call to {owner}.{} failed: {error}", info.name);
                        return Some(Value::NULL);
                    }
                };
                Some(match info.descriptor.rsplit(')').next() {
                    Some("V") => Value::NULL,
                    Some("Z") => self.box_field_value("Z", value),
                    _ => value,
                })
            }
            _ => None,
        }
    }

    fn reflect_field_call(&mut self, method_name: &str, args: &[Value]) -> Option<Value> {
        let field_id = args[0].as_obj()?;
        if !matches!(
            self.heap.get(field_id)?,
            JavaObject::Instance { class_name, .. } if class_name == "java/lang/reflect/Field"
        ) {
            return None;
        }
        let (owner, info) = self.field_of(&args[0])?;
        match method_name {
            "getName" => Some(Value::Object(Some(self.heap.alloc_string(info.name)))),
            "getModifiers" => Some(Value::Int(info.access as i32)),
            "getType" => Some(Value::Object(Some(
                self.heap.alloc_class(type_name(&info.descriptor)),
            ))),
            "getGenericType" => {
                // A field declared with type arguments reports a
                // `ParameterizedType`; any other field reports its class.
                let arguments = info
                    .signature
                    .as_deref()
                    .map(signature_type_arguments)
                    .unwrap_or_default();
                if arguments.is_empty() {
                    return Some(Value::Object(Some(
                        self.heap.alloc_class(type_name(&info.descriptor)),
                    )));
                }
                let classes: Vec<Value> = arguments
                    .into_iter()
                    .map(|name| Value::Object(Some(self.heap.alloc_class(name))))
                    .collect();
                let array = self.heap.alloc(JavaObject::Array {
                    elem_type: "java/lang/reflect/Type".into(),
                    elements: classes,
                });
                let mut fields = HashMap::new();
                fields.insert("arguments".to_string(), Value::Object(Some(array)));
                fields.insert(
                    "raw".to_string(),
                    Value::Object(Some(self.heap.alloc_class(type_name(&info.descriptor)))),
                );
                let id = self.heap.alloc(JavaObject::Instance {
                    class_name: "java/lang/reflect/ParameterizedType".into(),
                    fields,
                });
                Some(Value::Object(Some(id)))
            }
            "getDeclaringClass" => Some(Value::Object(Some(self.heap.alloc_class(owner)))),
            "setAccessible" => Some(Value::NULL),
            "trySetAccessible" | "canAccess" | "isAccessible" => Some(Value::from_bool(true)),
            "isSynthetic" | "isEnumConstant" => Some(Value::from_bool(false)),
            "get" | "getBoolean" | "getInt" | "getLong" | "getFloat" | "getDouble" | "getByte"
            | "getShort" | "getChar"
                if args.len() == 2 =>
            {
                let value = if info.is_static {
                    self.ensure_initialized(&owner).ok()?;
                    self.loader
                        .get_or_load_mut(&owner)
                        .ok()??
                        .static_fields
                        .get(&info.name)
                        .cloned()
                        .unwrap_or_else(|| default_value(&info.descriptor))
                } else {
                    match self.heap.get(args[1].as_obj()?)? {
                        JavaObject::Instance { fields, .. } => fields
                            .get(&info.name)
                            .cloned()
                            .unwrap_or_else(|| default_value(&info.descriptor)),
                        _ => return None,
                    }
                };
                Some(if method_name == "get" {
                    self.box_field_value(&info.descriptor, value)
                } else {
                    value
                })
            }
            "set" | "setBoolean" | "setInt" | "setLong" | "setFloat" | "setDouble" | "setByte"
            | "setShort" | "setChar"
                if args.len() == 3 =>
            {
                let value = self.unbox_field_value(&info.descriptor, &args[2]);
                if info.is_static {
                    self.ensure_initialized(&owner).ok()?;
                    self.loader
                        .get_or_load_mut(&owner)
                        .ok()??
                        .static_fields
                        .insert(info.name, value);
                } else if let JavaObject::Instance { fields, .. } =
                    self.heap.get_mut(args[1].as_obj()?)?
                {
                    fields.insert(info.name, value);
                }
                Some(Value::NULL)
            }
            "getAnnotation" | "getDeclaredAnnotation" | "isAnnotationPresent"
                if args.len() == 2 =>
            {
                let wanted = self.class_arg(&args[1])?;
                let found = info.annotations.iter().find(|a| a.type_name == wanted);
                if method_name == "isAnnotationPresent" {
                    return Some(Value::from_bool(found.is_some()));
                }
                match found.cloned() {
                    Some(annotation) => Some(self.annotation_object(&annotation)),
                    None => Some(Value::NULL),
                }
            }
            _ => None,
        }
    }
}
