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
}

/// Information about a class method.
#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub name: String,
    pub descriptor: String,
    pub is_static: bool,
    pub code: Option<Arc<CodeInfo>>,
}

/// Information about a class field.
#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub descriptor: String,
    pub is_static: bool,
}

/// Information about an invokedynamic bootstrap method.
#[derive(Debug, Clone)]
pub struct BootstrapMethodInfo {
    pub target_class: String,
    pub target_method: String,
    pub target_desc: String,
}

/// A parsed JVM class.
#[derive(Debug, Clone)]
pub struct Class {
    pub this_class: String,
    pub super_class: Option<String>,
    pub interfaces: Vec<String>,
    pub fields: HashMap<String, FieldInfo>,
    pub static_fields: HashMap<String, Value>,
    pub methods: HashMap<(String, String), MethodInfo>,
    pub bootstrap_methods: Vec<BootstrapMethodInfo>,
    pub static_initialized: bool,
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
        for f in &parsed.fields {
            let is_static = f.access_flags.contains(cafebabe::FieldAccessFlags::STATIC);
            fields.insert(
                f.name.to_string(),
                FieldInfo {
                    name: f.name.to_string(),
                    descriptor: f.descriptor.to_string(),
                    is_static,
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
                    for arg in &entry.arguments {
                        if let BootstrapArgument::MethodHandle(mh) = arg {
                            target_class = mh.class_name.to_string();
                            target_method = mh.member_ref.name.to_string();
                            target_desc = mh.member_ref.descriptor.to_string();
                            break;
                        }
                    }
                    bootstrap_methods.push(BootstrapMethodInfo {
                        target_class,
                        target_method,
                        target_desc,
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
                    }));
                }
            }
            methods.insert(
                (m.name.to_string(), m.descriptor.to_string()),
                MethodInfo {
                    name: m.name.to_string(),
                    descriptor: m.descriptor.to_string(),
                    is_static,
                    code,
                },
            );
        }

        Ok(Self {
            this_class,
            super_class,
            interfaces,
            fields,
            static_fields,
            methods,
            bootstrap_methods,
            static_initialized: false,
        })
    }
}

fn obj_arr_to_string(o: &ObjectArrayType<'_>) -> String {
    match o {
        ObjectArrayType::BinaryName(name) => name.to_string(),
        ObjectArrayType::ArrayType(desc) => format!("{desc:?}"),
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
        RawOpcode::Newarray(t) => Opcode::Newarray(format!("{t:?}")),
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
