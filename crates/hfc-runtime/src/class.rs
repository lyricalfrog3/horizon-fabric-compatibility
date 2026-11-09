//! Class file definitions and owned opcode IR for the runtime engine.

use crate::value::Value;
use cafebabe::attributes::AttributeData;
use cafebabe::bytecode::Opcode as RawOpcode;
use cafebabe::constant_pool::{BootstrapArgument, LiteralConstant, Loadable, ObjectArrayType};
use cafebabe::parse_class;
use hfc_core::{Error, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// An owned opcode for the execution engine.
#[derive(Debug, Clone, PartialEq)]
pub enum Opcode {
    Aload(u16),
    Astore(u16),
    Iload(u16),
    Istore(u16),
    Fload(u16),
    Fstore(u16),
    Lload(u16),
    Lstore(u16),
    Dload(u16),
    Dstore(u16),

    AconstNull,
    Iconst(i32),
    Fconst(f32),
    Lconst(i64),
    Dconst(f64),
    Bipush(i8),
    Sipush(i16),

    LdcString(String),
    LdcInt(i32),
    LdcFloat(f32),
    LdcLong(i64),
    LdcDouble(f64),
    LdcClass(String),

    Dup,
    DupX1,
    DupX2,
    Dup2,
    Dup2X1,
    Dup2X2,
    Pop,
    Pop2,
    Swap,

    Iadd,
    Isub,
    Imul,
    Idiv,
    Irem,
    Ineg,
    Iinc(u16, i32),

    Fadd,
    Fsub,
    Fmul,
    Fdiv,

    Ifeq(i32),
    Ifne(i32),
    Iflt(i32),
    Ifge(i32),
    Ifgt(i32),
    Ifle(i32),

    IfIcmpeq(i32),
    IfIcmpne(i32),
    IfIcmplt(i32),
    IfIcmpge(i32),
    IfIcmpgt(i32),
    IfIcmple(i32),

    IfAcmpeq(i32),
    IfAcmpne(i32),

    IfNull(i32),
    IfNonNull(i32),

    Goto(i32),
    Tableswitch {
        default: i32,
        low: i32,
        high: i32,
        offsets: Vec<i32>,
    },
    Lookupswitch {
        default: i32,
        pairs: Vec<(i32, i32)>,
    },

    Return,
    Ireturn,
    Lreturn,
    Freturn,
    Dreturn,
    Areturn,

    Getstatic {
        class_name: String,
        field_name: String,
        descriptor: String,
    },
    Putstatic {
        class_name: String,
        field_name: String,
        descriptor: String,
    },
    Getfield {
        class_name: String,
        field_name: String,
        descriptor: String,
    },
    Putfield {
        class_name: String,
        field_name: String,
        descriptor: String,
    },

    Invokestatic {
        class_name: String,
        method_name: String,
        descriptor: String,
    },
    Invokevirtual {
        class_name: String,
        method_name: String,
        descriptor: String,
    },
    Invokespecial {
        class_name: String,
        method_name: String,
        descriptor: String,
    },
    Invokeinterface {
        class_name: String,
        method_name: String,
        descriptor: String,
    },
    Invokedynamic {
        bootstrap_idx: u16,
        name: String,
        descriptor: String,
    },

    New(String),
    Newarray(String),
    Anewarray(String),
    Arraylength,
    Aaload,
    Aastore,
    Baload,
    Bastore,
    Iaload,
    Iastore,

    Ishl,
    Ishr,
    Iushr,
    Iand,
    Ior,
    Ixor,

    Ladd,
    Lsub,
    Lmul,
    Ldiv,
    Lrem,
    Lneg,
    Lshl,
    Lshr,
    Lushr,
    Land,
    Lor,
    Lxor,

    Fneg,

    Dadd,
    Dsub,
    Dmul,
    Ddiv,
    Drem,
    Dneg,

    Lcmp,
    Fcmpl,
    Fcmpg,
    Dcmpl,
    Dcmpg,

    I2l,
    I2f,
    I2d,
    L2i,
    L2f,
    L2d,
    F2i,
    F2l,
    F2d,
    D2i,
    D2l,
    D2f,
    I2b,
    I2c,
    I2s,

    Saload,
    Caload,
    Laload,
    Faload,
    Daload,
    Sastore,
    Castore,
    Lastore,
    Fastore,
    Dastore,

    Monitorenter,
    Monitorexit,
    Multianewarray(String, u8),

    Checkcast(String),
    Instanceof(String),
    Athrow,
    Nop,
}

/// Information about a method's code attribute.
#[derive(Debug, Clone)]
pub struct CodeInfo {
    pub max_stack: u16,
    pub max_locals: u16,
    pub opcodes: Vec<(usize, Opcode)>,
    /// The Code attribute's exception table, in declaration order (the first
    /// matching entry wins).
    pub handlers: Vec<ExceptionHandler>,
}

/// One `exception_table` entry. Offsets are bytecode byte offsets; the range
/// is `[start, end)`.
#[derive(Debug, Clone)]
pub struct ExceptionHandler {
    pub start: usize,
    pub end: usize,
    pub handler: usize,
    /// The caught class, or `None` for a catch-all (`finally`).
    pub catch_type: Option<String>,
}

/// Information about a class method.
#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub descriptor: String,
    pub is_static: bool,
    /// The class-file access flags (`ACC_PUBLIC` is 0x1).
    pub access: u16,
    pub code: Option<Arc<CodeInfo>>,
    /// Annotations on the method, including Mixin's `@Inject`.
    pub annotations: Vec<AnnotationInfo>,
    /// Annotations on each parameter, in parameter order.
    pub parameter_annotations: Vec<Vec<AnnotationInfo>>,
}

/// One class-file annotation, with its element values.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationInfo {
    /// The annotation type as a class name (`Lpkg/Name;` stripped).
    pub type_name: String,
    pub elements: Vec<(String, AnnotationValue)>,
}

impl AnnotationInfo {
    pub fn element(&self, name: &str) -> Option<&AnnotationValue> {
        self.elements
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    pub fn string(&self, name: &str) -> Option<&str> {
        match self.element(name)? {
            AnnotationValue::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn strings(&self, name: &str) -> Vec<&str> {
        match self.element(name) {
            Some(AnnotationValue::String(value)) => vec![value.as_str()],
            Some(AnnotationValue::Array(values)) => values
                .iter()
                .filter_map(|value| match value {
                    AnnotationValue::String(value) => Some(value.as_str()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn annotation(&self, name: &str) -> Option<&AnnotationInfo> {
        match self.element(name)? {
            AnnotationValue::Annotation(value) => Some(value),
            _ => None,
        }
    }

    pub fn annotations(&self, name: &str) -> Vec<&AnnotationInfo> {
        match self.element(name) {
            Some(AnnotationValue::Annotation(value)) => vec![value],
            Some(AnnotationValue::Array(values)) => values
                .iter()
                .filter_map(|value| match value {
                    AnnotationValue::Annotation(value) => Some(value),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// A class-file annotation element value.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationValue {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Bool(bool),
    Char(char),
    String(String),
    Enum { type_name: String, name: String },
    Class(String),
    Annotation(AnnotationInfo),
    Array(Vec<AnnotationValue>),
}

/// A class name from a `Lpkg/Name;` descriptor, or the name unchanged.
fn annotation_class_name(name: &str) -> String {
    name.strip_prefix('L')
        .and_then(|name| name.strip_suffix(';'))
        .unwrap_or(name)
        .replace('.', "/")
}

fn convert_annotation(annotation: &cafebabe::attributes::Annotation<'_>) -> AnnotationInfo {
    use cafebabe::attributes::AnnotationElementValue;

    fn convert_value(value: &AnnotationElementValue<'_>) -> AnnotationValue {
        match value {
            AnnotationElementValue::ByteConstant(value)
            | AnnotationElementValue::ShortConstant(value)
            | AnnotationElementValue::IntConstant(value) => AnnotationValue::Int(*value),
            AnnotationElementValue::CharConstant(value) => {
                AnnotationValue::Char(char::from_u32(*value as u32).unwrap_or('\0'))
            }
            AnnotationElementValue::LongConstant(value) => AnnotationValue::Long(*value),
            AnnotationElementValue::FloatConstant(value) => AnnotationValue::Float(*value),
            AnnotationElementValue::DoubleConstant(value) => AnnotationValue::Double(*value),
            AnnotationElementValue::BooleanConstant(value) => AnnotationValue::Bool(*value != 0),
            AnnotationElementValue::StringConstant(value) => {
                AnnotationValue::String(value.to_string())
            }
            AnnotationElementValue::EnumConstant {
                type_name,
                const_name,
            } => AnnotationValue::Enum {
                type_name: annotation_class_name(&type_name.to_string()),
                name: const_name.to_string(),
            },
            AnnotationElementValue::ClassLiteral { class_name } => {
                AnnotationValue::Class(annotation_class_name(class_name))
            }
            AnnotationElementValue::AnnotationValue(annotation) => {
                AnnotationValue::Annotation(convert_annotation(annotation))
            }
            AnnotationElementValue::ArrayValue(values) => {
                AnnotationValue::Array(values.iter().map(convert_value).collect())
            }
        }
    }

    AnnotationInfo {
        type_name: annotation_class_name(&annotation.type_descriptor.to_string()),
        elements: annotation
            .elements
            .iter()
            .map(|element| (element.name.to_string(), convert_value(&element.value)))
            .collect(),
    }
}

fn annotation_attributes(
    attributes: &[cafebabe::attributes::AttributeInfo<'_>],
) -> Vec<AnnotationInfo> {
    use cafebabe::attributes::AttributeData;
    let mut out = Vec::new();
    for attribute in attributes {
        match &attribute.data {
            AttributeData::RuntimeVisibleAnnotations(annotations)
            | AttributeData::RuntimeInvisibleAnnotations(annotations) => {
                out.extend(annotations.iter().map(convert_annotation));
            }
            _ => {}
        }
    }
    out
}

fn parameter_annotation_attributes(
    attributes: &[cafebabe::attributes::AttributeInfo<'_>],
) -> Vec<Vec<AnnotationInfo>> {
    use cafebabe::attributes::AttributeData;
    let mut out: Vec<Vec<AnnotationInfo>> = Vec::new();
    for attribute in attributes {
        let annotations = match &attribute.data {
            AttributeData::RuntimeVisibleParameterAnnotations(annotations)
            | AttributeData::RuntimeInvisibleParameterAnnotations(annotations) => annotations,
            _ => continue,
        };
        for (index, parameter) in annotations.iter().enumerate() {
            if out.len() <= index {
                out.resize(index + 1, Vec::new());
            }
            out[index].extend(parameter.annotations.iter().map(convert_annotation));
        }
    }
    out
}

/// Information about a class field.
#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub descriptor: String,
    pub is_static: bool,
    /// The class-file access flags (`ACC_PUBLIC` is 0x1).
    pub access: u16,
    /// Position in the class file, which is declaration order.
    pub index: usize,
    pub annotations: Vec<AnnotationInfo>,
    /// The generic `Signature` attribute (`Lfoo/Bar<Lbaz/Qux;>;`), when present.
    pub signature: Option<String>,
}

/// Information about an invokedynamic bootstrap method.
#[derive(Debug, Clone)]
pub struct BootstrapMethodInfo {
    /// The bootstrap method itself (`java/lang/invoke/LambdaMetafactory`).
    pub bootstrap_class: String,
    /// Its name (`altMetafactory`).
    pub bootstrap_method: String,
    pub target_class: String,
    pub target_method: String,
    pub target_desc: String,
    /// String constants in the bootstrap arguments. `StringConcatFactory` puts
    /// its recipe here; other bootstrap methods can ignore them.
    pub string_arguments: Vec<String>,
    /// Integer constants in the bootstrap arguments, in order:
    /// `LambdaMetafactory.altMetafactory` puts its flags and marker count here.
    pub int_arguments: Vec<i32>,
    /// Class constants in the bootstrap arguments, in order: the interfaces
    /// `altMetafactory` adds to the lambda.
    pub class_arguments: Vec<String>,
}

/// A parsed JVM class.
#[derive(Debug, Clone)]
pub struct Class {
    pub this_class: String,
    pub super_class: Option<String>,
    pub interfaces: Vec<String>,
    /// Whether the class file is an interface (`ACC_INTERFACE`).
    pub is_interface: bool,
    pub fields: HashMap<String, FieldInfo>,
    pub static_fields: HashMap<String, Value>,
    pub methods: HashMap<(String, String), MethodInfo>,
    pub bootstrap_methods: Vec<BootstrapMethodInfo>,
    pub static_initialized: bool,
    /// Class-level annotations (`@Mixin` on a mixin class).
    pub annotations: Vec<AnnotationInfo>,
}

/// True for a value that is already a class name rather than a descriptor.
fn is_class_name(name: &str) -> bool {
    !name.starts_with('[') && !name.starts_with('L') && !name.contains(';')
}

/// Extract every `Lpkg/Class;` reference from a descriptor or class name.
fn collect_descriptor_classes(descriptor: &str, out: &mut std::collections::BTreeSet<String>) {
    if matches!(
        descriptor,
        "void" | "boolean" | "byte" | "char" | "short" | "int" | "long" | "float" | "double"
    ) {
        return;
    }
    if is_class_name(descriptor) {
        if !descriptor.is_empty() {
            out.insert(descriptor.to_string());
        }
        return;
    }
    let bytes = descriptor.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'L' {
            if let Some(end) = descriptor[i + 1..].find(';') {
                let name = &descriptor[i + 1..i + 1 + end];
                if !name.is_empty() {
                    out.insert(name.to_string());
                }
                i += end + 2;
                continue;
            }
        }
        i += 1;
    }
}

/// Every class named by a raw class file's constant pool.
///
/// This scans the pool directly instead of building a whole `Class`, so a
/// startup dependency walk can cover tens of thousands of classes cheaply.
pub fn scan_constant_pool_classes(bytes: &[u8]) -> Vec<String> {
    let Some(count) = be_u16(bytes, 8) else {
        return Vec::new();
    };
    let mut out = std::collections::BTreeSet::new();
    let mut slot = 1;
    let mut offset = 10;
    while slot < count {
        let Some(&tag) = bytes.get(offset) else {
            break;
        };
        offset += 1;
        match tag {
            1 => {
                let Some(len) = be_u16(bytes, offset) else {
                    break;
                };
                offset += 2 + len as usize;
            }
            7 => {
                let Some(index) = be_u16(bytes, offset) else {
                    break;
                };
                if let Some(name) = resolve_utf8_slot(bytes, count, index as usize) {
                    if let Some(name) = normalize_class_ref(&name) {
                        out.insert(name);
                    }
                }
                offset += 2;
            }
            8 | 16 | 19 | 20 => offset += 2,
            15 => offset += 3,
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => offset += 4,
            5 | 6 => {
                offset += 8;
                // Eight-byte constants reserve the following pool slot.
                slot += 1;
            }
            _ => break,
        }
        slot += 1;
    }
    out.into_iter().collect()
}

/// Every method reference (`Methodref`/`InterfaceMethodref`) in a raw class
/// file's constant pool, as `(owner class, method name, descriptor)`.
pub fn scan_method_refs(bytes: &[u8]) -> Vec<(String, String, String)> {
    let Some(count) = be_u16(bytes, 8) else {
        return Vec::new();
    };
    let mut classes: std::collections::HashMap<u16, String> = Default::default();
    let mut names: std::collections::HashMap<u16, (String, String)> = Default::default();
    let mut refs: Vec<(u16, u16)> = Vec::new();
    let mut slot = 1u16;
    let mut offset = 10;
    while slot < count {
        let Some(&tag) = bytes.get(offset) else {
            break;
        };
        offset += 1;
        match tag {
            1 => {
                let Some(len) = be_u16(bytes, offset) else {
                    break;
                };
                offset += 2 + len as usize;
            }
            7 => {
                let Some(index) = be_u16(bytes, offset) else {
                    break;
                };
                if let Some(name) = resolve_utf8_slot(bytes, count, index as usize) {
                    classes.insert(slot, name);
                }
                offset += 2;
            }
            8 | 16 | 19 | 20 => offset += 2,
            15 => offset += 3,
            12 => {
                let (Some(name), Some(descriptor)) =
                    (be_u16(bytes, offset), be_u16(bytes, offset + 2))
                else {
                    break;
                };
                if let (Some(name), Some(descriptor)) = (
                    resolve_utf8_slot(bytes, count, name as usize),
                    resolve_utf8_slot(bytes, count, descriptor as usize),
                ) {
                    names.insert(slot, (name, descriptor));
                }
                offset += 4;
            }
            9 | 10 | 11 => {
                let (Some(owner), Some(name)) = (be_u16(bytes, offset), be_u16(bytes, offset + 2))
                else {
                    break;
                };
                // Field references (9) are not method calls.
                if tag != 9 {
                    refs.push((owner, name));
                }
                offset += 4;
            }
            3 | 4 | 17 | 18 => offset += 4,
            5 | 6 => {
                offset += 8;
                slot += 1;
            }
            _ => break,
        }
        slot += 1;
    }
    let mut out = Vec::new();
    for (owner, name) in refs {
        let (Some(owner), Some((name, descriptor))) = (classes.get(&owner), names.get(&name))
        else {
            continue;
        };
        if let Some(owner) = normalize_class_ref(owner) {
            out.push((owner, name.clone(), descriptor.clone()));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Turn a constant-pool class reference into a class name. Arrays become their
/// element type (`[Ljava/lang/String;` → `java/lang/String`); primitive arrays
/// have no class file and are dropped.
fn normalize_class_ref(name: &str) -> Option<String> {
    let mut name = name;
    while let Some(stripped) = name.strip_prefix('[') {
        name = stripped;
    }
    if name.len() == 1 {
        return None;
    }
    if let Some(inner) = name.strip_prefix('L').and_then(|n| n.strip_suffix(';')) {
        return Some(inner.to_string());
    }
    Some(name.to_string())
}

fn be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
    ]))
}

fn resolve_utf8_slot(bytes: &[u8], count: u16, wanted: usize) -> Option<String> {
    let mut slot: usize = 1;
    let mut offset = 10;
    while slot < count as usize {
        let &tag = bytes.get(offset)?;
        offset += 1;
        match tag {
            1 => {
                let len = be_u16(bytes, offset)? as usize;
                offset += 2;
                if slot == wanted {
                    let raw = bytes.get(offset..offset + len)?;
                    return Some(String::from_utf8_lossy(raw).into_owned());
                }
                offset += len;
            }
            7 | 8 | 16 | 19 | 20 => offset += 2,
            15 => offset += 3,
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => offset += 4,
            5 | 6 => {
                offset += 8;
                slot += 1;
            }
            _ => return None,
        }
        slot += 1;
    }
    None
}

impl Class {
    /// Every class this class names: its superclass, interfaces, field and
    /// method types, and every class appearing in its bytecode.
    ///
    /// The runtime uses this to compute the transitive closure a mod needs
    /// instead of hardcoding a list of shims.
    pub fn referenced_classes(&self) -> Vec<String> {
        let mut out = std::collections::BTreeSet::new();
        if let Some(super_class) = &self.super_class {
            out.insert(super_class.clone());
        }
        for interface in &self.interfaces {
            out.insert(interface.clone());
        }
        for field in self.fields.values() {
            collect_descriptor_classes(&field.descriptor, &mut out);
        }
        for ((_, descriptor), method) in &self.methods {
            collect_descriptor_classes(descriptor, &mut out);
            let Some(code) = &method.code else {
                continue;
            };
            for (_, op) in &code.opcodes {
                match op {
                    Opcode::New(name)
                    | Opcode::Newarray(name)
                    | Opcode::Anewarray(name)
                    | Opcode::Checkcast(name)
                    | Opcode::Instanceof(name)
                    | Opcode::LdcClass(name) => {
                        if is_class_name(name) {
                            out.insert(name.clone());
                        } else {
                            collect_descriptor_classes(name, &mut out);
                        }
                    }
                    Opcode::Multianewarray(name, _) => collect_descriptor_classes(name, &mut out),
                    Opcode::Getstatic { class_name, .. }
                    | Opcode::Putstatic { class_name, .. }
                    | Opcode::Getfield { class_name, .. }
                    | Opcode::Putfield { class_name, .. }
                    | Opcode::Invokestatic { class_name, .. }
                    | Opcode::Invokevirtual { class_name, .. }
                    | Opcode::Invokespecial { class_name, .. }
                    | Opcode::Invokeinterface { class_name, .. } => {
                        collect_descriptor_classes(class_name, &mut out);
                    }
                    Opcode::Invokedynamic { descriptor, .. } => {
                        collect_descriptor_classes(descriptor, &mut out);
                    }
                    _ => {}
                }
            }
        }
        for bootstrap in &self.bootstrap_methods {
            collect_descriptor_classes(&bootstrap.target_class, &mut out);
            collect_descriptor_classes(&bootstrap.target_desc, &mut out);
        }
        out.into_iter().collect()
    }

    /// Parse class bytes into a runtime Class structure.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let parsed = parse_class(bytes)
            .map_err(|e| Error::new("class", format!("failed to parse class: {e:?}")))?;

        let this_class = parsed.this_class.to_string();
        let super_class = parsed.super_class.map(|s| s.to_string());
        let interfaces = parsed.interfaces.iter().map(|s| s.to_string()).collect();

        let mut fields = HashMap::new();
        let mut static_fields = HashMap::new();
        for (index, f) in parsed.fields.iter().enumerate() {
            let is_static = f.access_flags.contains(cafebabe::FieldAccessFlags::STATIC);
            fields.insert(
                f.name.to_string(),
                FieldInfo {
                    name: f.name.to_string(),
                    descriptor: f.descriptor.to_string(),
                    is_static,
                    access: f.access_flags.bits(),
                    index,
                    annotations: annotation_attributes(&f.attributes),
                    signature: f.attributes.iter().find_map(|a| match &a.data {
                        cafebabe::attributes::AttributeData::Signature(sig) => {
                            Some(sig.to_string())
                        }
                        _ => None,
                    }),
                },
            );
            if is_static {
                static_fields.insert(f.name.to_string(), Value::NULL);
            }
        }

        let mut bootstrap_methods = Vec::new();
        for attr in &parsed.attributes {
            if let AttributeData::BootstrapMethods(bsm) = &attr.data {
                for entry in bsm {
                    // Extract target method handle from arguments (index 1 is standard lambda target)
                    let mut target_class = String::new();
                    let mut target_method = String::new();
                    let mut target_desc = String::new();
                    let mut string_arguments = Vec::new();
                    let mut int_arguments = Vec::new();
                    let mut class_arguments = Vec::new();
                    // `StringConcatFactory` is the bootstrap method itself; the
                    // method handle inside the arguments is the concat target.
                    if entry.method.class_name == "java/lang/invoke/StringConcatFactory" {
                        target_class = entry.method.class_name.to_string();
                        target_method = entry.method.member_ref.name.to_string();
                        target_desc = entry.method.member_ref.descriptor.to_string();
                    }
                    for arg in &entry.arguments {
                        match arg {
                            BootstrapArgument::MethodHandle(mh) => {
                                if target_class.is_empty() {
                                    target_class = mh.class_name.to_string();
                                    target_method = mh.member_ref.name.to_string();
                                    target_desc = mh.member_ref.descriptor.to_string();
                                }
                            }
                            BootstrapArgument::LiteralConstant(LiteralConstant::Integer(i)) => {
                                int_arguments.push(*i);
                            }
                            BootstrapArgument::ClassInfo(name) => {
                                class_arguments.push(name.to_string());
                            }
                            BootstrapArgument::LiteralConstant(LiteralConstant::String(s)) => {
                                string_arguments.push(s.to_string());
                            }
                            BootstrapArgument::LiteralConstant(LiteralConstant::StringBytes(
                                bytes,
                            )) => {
                                string_arguments.push(String::from_utf8_lossy(bytes).into_owned());
                            }
                            _ => {}
                        }
                    }
                    bootstrap_methods.push(BootstrapMethodInfo {
                        bootstrap_class: entry.method.class_name.to_string(),
                        bootstrap_method: entry.method.member_ref.name.to_string(),
                        target_class,
                        target_method,
                        target_desc,
                        string_arguments,
                        int_arguments,
                        class_arguments,
                    });
                }
            }
        }

        let mut methods = HashMap::new();
        for m in &parsed.methods {
            let is_static = m.access_flags.contains(cafebabe::MethodAccessFlags::STATIC);
            let mut code = None;
            for attr in &m.attributes {
                if let AttributeData::Code(raw_code) = &attr.data {
                    let mut opcodes = Vec::new();
                    if let Some(bc) = &raw_code.bytecode {
                        for (offset, raw_op) in &bc.opcodes {
                            if let Some(op) = convert_opcode(raw_op) {
                                opcodes.push((*offset, op));
                            }
                        }
                    }
                    code = Some(Arc::new(CodeInfo {
                        max_stack: raw_code.max_stack,
                        max_locals: raw_code.max_locals,
                        opcodes,
                        handlers: raw_code
                            .exception_table
                            .iter()
                            .map(|entry| ExceptionHandler {
                                start: entry.start_pc as usize,
                                end: entry.end_pc as usize,
                                handler: entry.handler_pc as usize,
                                catch_type: entry.catch_type.as_ref().map(|t| t.to_string()),
                            })
                            .collect(),
                    }));
                }
            }
            methods.insert(
                (m.name.to_string(), m.descriptor.to_string()),
                MethodInfo {
                    name: m.name.to_string(),
                    descriptor: m.descriptor.to_string(),
                    is_static,
                    access: m.access_flags.bits(),
                    code,
                    annotations: annotation_attributes(&m.attributes),
                    parameter_annotations: parameter_annotation_attributes(&m.attributes),
                },
            );
        }

        Ok(Self {
            this_class,
            super_class,
            interfaces,
            is_interface: parsed
                .access_flags
                .contains(cafebabe::ClassAccessFlags::INTERFACE),
            fields,
            static_fields,
            methods,
            bootstrap_methods,
            static_initialized: false,
            annotations: annotation_attributes(&parsed.attributes),
        })
    }
}

fn obj_arr_to_string(o: &ObjectArrayType<'_>) -> String {
    match o {
        ObjectArrayType::BinaryName(name) => name.to_string(),
        // The JVM descriptor (`[Ljava/lang/Object;`), which is what the
        // interpreter's `instanceof`/`checkcast` match against.
        ObjectArrayType::ArrayType(desc) => format!("{desc}"),
    }
}

fn convert_opcode(raw: &RawOpcode<'_>) -> Option<Opcode> {
    Some(match raw {
        RawOpcode::Aload(i) => Opcode::Aload(*i),
        RawOpcode::Astore(i) => Opcode::Astore(*i),
        RawOpcode::Iload(i) => Opcode::Iload(*i),
        RawOpcode::Istore(i) => Opcode::Istore(*i),
        RawOpcode::Fload(i) => Opcode::Fload(*i),
        RawOpcode::Fstore(i) => Opcode::Fstore(*i),
        RawOpcode::Lload(i) => Opcode::Lload(*i),
        RawOpcode::Lstore(i) => Opcode::Lstore(*i),
        RawOpcode::Dload(i) => Opcode::Dload(*i),
        RawOpcode::Dstore(i) => Opcode::Dstore(*i),

        RawOpcode::AconstNull => Opcode::AconstNull,
        RawOpcode::IconstM1 => Opcode::Iconst(-1),
        RawOpcode::Iconst0 => Opcode::Iconst(0),
        RawOpcode::Iconst1 => Opcode::Iconst(1),
        RawOpcode::Iconst2 => Opcode::Iconst(2),
        RawOpcode::Iconst3 => Opcode::Iconst(3),
        RawOpcode::Iconst4 => Opcode::Iconst(4),
        RawOpcode::Iconst5 => Opcode::Iconst(5),

        RawOpcode::Fconst0 => Opcode::Fconst(0.0),
        RawOpcode::Fconst1 => Opcode::Fconst(1.0),
        RawOpcode::Fconst2 => Opcode::Fconst(2.0),

        RawOpcode::Lconst0 => Opcode::Lconst(0),
        RawOpcode::Lconst1 => Opcode::Lconst(1),

        RawOpcode::Dconst0 => Opcode::Dconst(0.0),
        RawOpcode::Dconst1 => Opcode::Dconst(1.0),

        RawOpcode::Bipush(b) => Opcode::Bipush(*b),
        RawOpcode::Sipush(s) => Opcode::Sipush(*s),

        RawOpcode::Ldc(entry) | RawOpcode::LdcW(entry) => match entry {
            Loadable::LiteralConstant(lc) => match lc {
                LiteralConstant::String(s) => Opcode::LdcString(s.to_string()),
                LiteralConstant::Integer(i) => Opcode::LdcInt(*i),
                LiteralConstant::Float(f) => Opcode::LdcFloat(*f),
                LiteralConstant::Long(l) => Opcode::LdcLong(*l),
                LiteralConstant::Double(d) => Opcode::LdcDouble(*d),
                LiteralConstant::StringBytes(b) => {
                    Opcode::LdcString(String::from_utf8_lossy(b).to_string())
                }
            },
            Loadable::ClassInfo(c) => Opcode::LdcClass(c.to_string()),
            _ => return None,
        },
        RawOpcode::Ldc2W(entry) => match entry {
            Loadable::LiteralConstant(LiteralConstant::Long(l)) => Opcode::LdcLong(*l),
            Loadable::LiteralConstant(LiteralConstant::Double(d)) => Opcode::LdcDouble(*d),
            _ => return None,
        },

        RawOpcode::Dup => Opcode::Dup,
        RawOpcode::DupX1 => Opcode::DupX1,
        RawOpcode::DupX2 => Opcode::DupX2,
        RawOpcode::Dup2 => Opcode::Dup2,
        RawOpcode::Dup2X1 => Opcode::Dup2X1,
        RawOpcode::Dup2X2 => Opcode::Dup2X2,
        RawOpcode::Pop => Opcode::Pop,
        RawOpcode::Pop2 => Opcode::Pop2,
        RawOpcode::Swap => Opcode::Swap,

        RawOpcode::Iadd => Opcode::Iadd,
        RawOpcode::Isub => Opcode::Isub,
        RawOpcode::Imul => Opcode::Imul,
        RawOpcode::Idiv => Opcode::Idiv,
        RawOpcode::Irem => Opcode::Irem,
        RawOpcode::Ineg => Opcode::Ineg,
        RawOpcode::Iinc(idx, c) => Opcode::Iinc(*idx, *c as i32),

        RawOpcode::Fadd => Opcode::Fadd,
        RawOpcode::Fsub => Opcode::Fsub,
        RawOpcode::Fmul => Opcode::Fmul,
        RawOpcode::Fdiv => Opcode::Fdiv,
        RawOpcode::Fneg => Opcode::Fneg,

        RawOpcode::Ishl => Opcode::Ishl,
        RawOpcode::Ishr => Opcode::Ishr,
        RawOpcode::Iushr => Opcode::Iushr,
        RawOpcode::Iand => Opcode::Iand,
        RawOpcode::Ior => Opcode::Ior,
        RawOpcode::Ixor => Opcode::Ixor,

        RawOpcode::Ladd => Opcode::Ladd,
        RawOpcode::Lsub => Opcode::Lsub,
        RawOpcode::Lmul => Opcode::Lmul,
        RawOpcode::Ldiv => Opcode::Ldiv,
        RawOpcode::Lrem => Opcode::Lrem,
        RawOpcode::Lneg => Opcode::Lneg,
        RawOpcode::Lshl => Opcode::Lshl,
        RawOpcode::Lshr => Opcode::Lshr,
        RawOpcode::Lushr => Opcode::Lushr,
        RawOpcode::Land => Opcode::Land,
        RawOpcode::Lor => Opcode::Lor,
        RawOpcode::Lxor => Opcode::Lxor,

        RawOpcode::Dadd => Opcode::Dadd,
        RawOpcode::Dsub => Opcode::Dsub,
        RawOpcode::Dmul => Opcode::Dmul,
        RawOpcode::Ddiv => Opcode::Ddiv,
        RawOpcode::Drem => Opcode::Drem,
        RawOpcode::Dneg => Opcode::Dneg,

        RawOpcode::Lcmp => Opcode::Lcmp,
        RawOpcode::Fcmpl => Opcode::Fcmpl,
        RawOpcode::Fcmpg => Opcode::Fcmpg,
        RawOpcode::Dcmpl => Opcode::Dcmpl,
        RawOpcode::Dcmpg => Opcode::Dcmpg,

        RawOpcode::I2l => Opcode::I2l,
        RawOpcode::I2f => Opcode::I2f,
        RawOpcode::I2d => Opcode::I2d,
        RawOpcode::L2i => Opcode::L2i,
        RawOpcode::L2f => Opcode::L2f,
        RawOpcode::L2d => Opcode::L2d,
        RawOpcode::F2i => Opcode::F2i,
        RawOpcode::F2l => Opcode::F2l,
        RawOpcode::F2d => Opcode::F2d,
        RawOpcode::D2i => Opcode::D2i,
        RawOpcode::D2l => Opcode::D2l,
        RawOpcode::D2f => Opcode::D2f,
        RawOpcode::I2b => Opcode::I2b,
        RawOpcode::I2c => Opcode::I2c,
        RawOpcode::I2s => Opcode::I2s,

        RawOpcode::Saload => Opcode::Saload,
        RawOpcode::Caload => Opcode::Caload,
        RawOpcode::Laload => Opcode::Laload,
        RawOpcode::Faload => Opcode::Faload,
        RawOpcode::Daload => Opcode::Daload,
        RawOpcode::Sastore => Opcode::Sastore,
        RawOpcode::Castore => Opcode::Castore,
        RawOpcode::Lastore => Opcode::Lastore,
        RawOpcode::Fastore => Opcode::Fastore,
        RawOpcode::Dastore => Opcode::Dastore,

        RawOpcode::Monitorenter => Opcode::Monitorenter,
        RawOpcode::Monitorexit => Opcode::Monitorexit,
        RawOpcode::Multianewarray(ty, dims) => Opcode::Multianewarray(obj_arr_to_string(ty), *dims),

        RawOpcode::Ifeq(off) => Opcode::Ifeq(*off),
        RawOpcode::Ifne(off) => Opcode::Ifne(*off),
        RawOpcode::Iflt(off) => Opcode::Iflt(*off),
        RawOpcode::Ifge(off) => Opcode::Ifge(*off),
        RawOpcode::Ifgt(off) => Opcode::Ifgt(*off),
        RawOpcode::Ifle(off) => Opcode::Ifle(*off),

        RawOpcode::IfIcmpeq(off) => Opcode::IfIcmpeq(*off),
        RawOpcode::IfIcmpne(off) => Opcode::IfIcmpne(*off),
        RawOpcode::IfIcmplt(off) => Opcode::IfIcmplt(*off),
        RawOpcode::IfIcmpge(off) => Opcode::IfIcmpge(*off),
        RawOpcode::IfIcmpgt(off) => Opcode::IfIcmpgt(*off),
        RawOpcode::IfIcmple(off) => Opcode::IfIcmple(*off),

        RawOpcode::IfAcmpeq(off) => Opcode::IfAcmpeq(*off),
        RawOpcode::IfAcmpne(off) => Opcode::IfAcmpne(*off),

        RawOpcode::Ifnull(off) => Opcode::IfNull(*off),
        RawOpcode::Ifnonnull(off) => Opcode::IfNonNull(*off),

        RawOpcode::Goto(off) => Opcode::Goto(*off),
        RawOpcode::Tableswitch(ts) => Opcode::Tableswitch {
            default: ts.default,
            low: ts.low,
            high: ts.high,
            offsets: ts.jumps.clone(),
        },
        RawOpcode::Lookupswitch(ls) => Opcode::Lookupswitch {
            default: ls.default,
            pairs: ls.match_offsets.clone(),
        },

        RawOpcode::Return => Opcode::Return,
        RawOpcode::Ireturn => Opcode::Ireturn,
        RawOpcode::Lreturn => Opcode::Lreturn,
        RawOpcode::Freturn => Opcode::Freturn,
        RawOpcode::Dreturn => Opcode::Dreturn,
        RawOpcode::Areturn => Opcode::Areturn,

        RawOpcode::Getstatic(f) => Opcode::Getstatic {
            class_name: f.class_name.to_string(),
            field_name: f.name_and_type.name.to_string(),
            descriptor: f.name_and_type.descriptor.to_string(),
        },
        RawOpcode::Putstatic(f) => Opcode::Putstatic {
            class_name: f.class_name.to_string(),
            field_name: f.name_and_type.name.to_string(),
            descriptor: f.name_and_type.descriptor.to_string(),
        },
        RawOpcode::Getfield(f) => Opcode::Getfield {
            class_name: f.class_name.to_string(),
            field_name: f.name_and_type.name.to_string(),
            descriptor: f.name_and_type.descriptor.to_string(),
        },
        RawOpcode::Putfield(f) => Opcode::Putfield {
            class_name: f.class_name.to_string(),
            field_name: f.name_and_type.name.to_string(),
            descriptor: f.name_and_type.descriptor.to_string(),
        },

        RawOpcode::Invokestatic(m) => Opcode::Invokestatic {
            class_name: m.class_name.to_string(),
            method_name: m.name_and_type.name.to_string(),
            descriptor: m.name_and_type.descriptor.to_string(),
        },
        RawOpcode::Invokevirtual(m) => Opcode::Invokevirtual {
            class_name: m.class_name.to_string(),
            method_name: m.name_and_type.name.to_string(),
            descriptor: m.name_and_type.descriptor.to_string(),
        },
        RawOpcode::Invokespecial(m) => Opcode::Invokespecial {
            class_name: m.class_name.to_string(),
            method_name: m.name_and_type.name.to_string(),
            descriptor: m.name_and_type.descriptor.to_string(),
        },
        RawOpcode::Invokeinterface(m, _count) => Opcode::Invokeinterface {
            class_name: m.class_name.to_string(),
            method_name: m.name_and_type.name.to_string(),
            descriptor: m.name_and_type.descriptor.to_string(),
        },
        RawOpcode::Invokedynamic(id) => Opcode::Invokedynamic {
            bootstrap_idx: id.attr_index,
            name: id.name_and_type.name.to_string(),
            descriptor: id.name_and_type.descriptor.to_string(),
        },

        RawOpcode::New(c) => Opcode::New(c.to_string()),
        RawOpcode::Newarray(t) => Opcode::Newarray(format!("{t:?}").to_lowercase()),
        RawOpcode::Anewarray(c) => Opcode::Anewarray(obj_arr_to_string(c)),
        RawOpcode::Arraylength => Opcode::Arraylength,
        RawOpcode::Aaload => Opcode::Aaload,
        RawOpcode::Aastore => Opcode::Aastore,
        RawOpcode::Baload => Opcode::Baload,
        RawOpcode::Bastore => Opcode::Bastore,
        RawOpcode::Iaload => Opcode::Iaload,
        RawOpcode::Iastore => Opcode::Iastore,

        RawOpcode::Checkcast(c) => Opcode::Checkcast(obj_arr_to_string(c)),
        RawOpcode::Instanceof(c) => Opcode::Instanceof(obj_arr_to_string(c)),
        RawOpcode::Athrow => Opcode::Athrow,
        RawOpcode::Nop => Opcode::Nop,
        _ => return None,
    })
}
