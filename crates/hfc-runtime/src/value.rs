//! Values and object model for the JVM execution engine.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;

/// A JVM runtime value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Object(Option<usize>),
}

impl Value {
    pub const NULL: Self = Value::Object(None);

    pub fn from_bool(b: bool) -> Self {
        Value::Int(if b { 1 } else { 0 })
    }

    pub fn as_int(&self) -> i32 {
        match self {
            Value::Int(v) => *v,
            Value::Long(v) => *v as i32,
            Value::Float(v) => *v as i32,
            Value::Double(v) => *v as i32,
            Value::Object(Some(id)) => *id as i32,
            Value::Object(None) => 0,
        }
    }

    pub fn as_bool(&self) -> bool {
        self.as_int() != 0
    }

    pub fn as_obj(&self) -> Option<usize> {
        match self {
            Value::Object(opt) => *opt,
            _ => None,
        }
    }

    pub fn as_long(&self) -> i64 {
        match self {
            Value::Long(v) => *v,
            Value::Int(v) => *v as i64,
            Value::Float(v) => *v as i64,
            Value::Double(v) => *v as i64,
            Value::Object(Some(id)) => *id as i64,
            Value::Object(None) => 0,
        }
    }

    pub fn as_float(&self) -> f32 {
        match self {
            Value::Float(v) => *v,
            Value::Double(v) => *v as f32,
            Value::Int(v) => *v as f32,
            Value::Long(v) => *v as f32,
            Value::Object(_) => 0.0,
        }
    }

    pub fn as_double(&self) -> f64 {
        match self {
            Value::Double(v) => *v,
            Value::Float(v) => *v as f64,
            Value::Int(v) => *v as f64,
            Value::Long(v) => *v as f64,
            Value::Object(_) => 0.0,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Object(None))
    }
}

/// A trait for native host/Minecraft objects.
pub trait NativeObject: fmt::Debug + Send + Sync {
    fn class_name(&self) -> &str;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// A heap-allocated Java object.
pub enum JavaObject {
    /// Ordinary class instance with fields.
    Instance {
        class_name: String,
        fields: HashMap<String, Value>,
    },
    /// An array of primitive or reference values.
    Array {
        elem_type: String,
        elements: Vec<Value>,
    },
    /// A Java List (e.g. ArrayList).
    List(Vec<Value>),
    /// A Java Map (e.g. HashMap).
    Map(Vec<(Value, Value)>),
    /// A Java Set (e.g. HashSet).
    Set(Vec<Value>),
    /// A Java Stream (e.g. java.util.stream.Stream).
    Stream(Vec<Value>),
    /// A Java Iterator over a snapshot of `items`. `source` is the runtime
    /// list or set it walks, which `Iterator.remove` writes through to.
    Iterator {
        items: Vec<Value>,
        cursor: usize,
        source: Option<usize>,
    },
    /// A Java String.
    String(String),
    /// A `java.lang.Class` instance.
    Class(String),
    /// A lambda closure created via `invokedynamic` / `LambdaMetafactory`.
    Lambda {
        interface_class: String,
        /// The interface method the lambda implements (the `invokedynamic` name).
        interface_method: String,
        target_class: String,
        target_method: String,
        target_desc: String,
        captured: Vec<Value>,
    },
    /// A native object supplied by the bridge (e.g. BlockState, Level, BlockPos).
    Native(Box<dyn NativeObject>),
}

impl fmt::Debug for JavaObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JavaObject::Instance { class_name, fields } => f
                .debug_struct("Instance")
                .field("class_name", class_name)
                .field("fields", fields)
                .finish(),
            JavaObject::Array {
                elem_type,
                elements,
            } => f
                .debug_struct("Array")
                .field("elem_type", elem_type)
                .field("len", &elements.len())
                .finish(),
            JavaObject::List(items) => f.debug_tuple("List").field(&items.len()).finish(),
            JavaObject::Map(entries) => f.debug_tuple("Map").field(&entries.len()).finish(),
            JavaObject::Set(items) => f.debug_tuple("Set").field(&items.len()).finish(),
            JavaObject::Stream(items) => f.debug_tuple("Stream").field(&items.len()).finish(),
            JavaObject::Iterator { cursor, items, .. } => f
                .debug_struct("Iterator")
                .field("cursor", cursor)
                .field("len", &items.len())
                .finish(),
            JavaObject::String(s) => f.debug_tuple("String").field(s).finish(),
            JavaObject::Class(c) => f.debug_tuple("Class").field(c).finish(),
            JavaObject::Lambda {
                interface_class,
                target_method,
                ..
            } => f
                .debug_struct("Lambda")
                .field("interface", interface_class)
                .field("target", target_method)
                .finish(),
            JavaObject::Native(n) => f.debug_tuple("Native").field(n).finish(),
        }
    }
}

impl JavaObject {
    pub fn class_name(&self) -> &str {
        match self {
            JavaObject::Instance { class_name, .. } => class_name,
            JavaObject::Array { elem_type, .. } => elem_type,
            JavaObject::List(_) => "java/util/ArrayList",
            JavaObject::Map(_) => "java/util/HashMap",
            JavaObject::Set(_) => "java/util/HashSet",
            JavaObject::Stream(_) => "java/util/stream/Stream",
            JavaObject::Iterator { .. } => "java/util/Iterator",
            JavaObject::String(_) => "java/lang/String",
            JavaObject::Class(_) => "java/lang/Class",
            JavaObject::Lambda {
                interface_class, ..
            } => interface_class,
            JavaObject::Native(n) => n.class_name(),
        }
    }
}
