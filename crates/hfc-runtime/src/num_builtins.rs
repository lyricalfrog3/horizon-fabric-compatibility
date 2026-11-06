//! Pure numeric and character statics of `Integer`, `Long`, `Double` and
//! `Character`. They have no host dependency, so they run natively.

use crate::builtins::BuiltinResult;
use crate::heap::Heap;
use crate::value::Value;

fn string_of(heap: &Heap, v: &Value) -> Option<String> {
    heap.get_string(v.as_obj()?).map(str::to_string)
}

pub fn numeric_method(
    heap: &mut Heap,
    class_name: &str,
    method_name: &str,
    descriptor: &str,
    args: &[Value],
) -> Option<BuiltinResult> {
    let handled = |v: Value| Some(BuiltinResult::Handled(v));
    let int = |i: usize| args.get(i).map(Value::as_int);
    let long = |i: usize| args.get(i).map(Value::as_long);

    // Boxed numbers are the plain values (see `Number` below), so boxing returns
    // its argument and an unboxing accessor converts it. A box that was made by
    // the JDK's own constructor holds its number in a `value` field; read that.
    if matches!(
        class_name,
        "java/lang/Double"
            | "java/lang/Float"
            | "java/lang/Long"
            | "java/lang/Short"
            | "java/lang/Byte"
    ) {
        let unboxed = |v: &Value| match v.as_obj().and_then(|id| heap.get(id)) {
            Some(crate::value::JavaObject::Instance { fields, .. }) => {
                fields.get("value").cloned().unwrap_or_else(|| v.clone())
            }
            _ => v.clone(),
        };
        match (method_name, descriptor.as_bytes().get(1)) {
            ("valueOf", Some(b'D' | b'F' | b'J' | b'S' | b'B')) => {
                return handled(args.first()?.clone());
            }
            ("doubleValue", Some(b')')) => {
                return handled(Value::Double(unboxed(args.first()?).as_double()));
            }
            ("floatValue", Some(b')')) => {
                return handled(Value::Float(unboxed(args.first()?).as_float()));
            }
            ("longValue", Some(b')')) => {
                return handled(Value::Long(unboxed(args.first()?).as_long()));
            }
            ("intValue", Some(b')')) => {
                return handled(Value::Int(unboxed(args.first()?).as_int()));
            }
            _ => {}
        }
    }

    match class_name {
        "java/util/concurrent/TimeUnit" if method_name == "convert" && args.len() == 3 => {
            // convert(duration, sourceUnit): `this` is the target unit.
            let nanos = |v: &Value| -> Option<i128> {
                let id = v.as_obj()?;
                let name = match heap.get(id)? {
                    crate::value::JavaObject::Instance { fields, .. } => {
                        string_of(heap, fields.get("name")?)?
                    }
                    _ => return None,
                };
                Some(match name.as_str() {
                    "NANOSECONDS" => 1,
                    "MICROSECONDS" => 1_000,
                    "MILLISECONDS" => 1_000_000,
                    "SECONDS" => 1_000_000_000,
                    "MINUTES" => 60_000_000_000,
                    "HOURS" => 3_600_000_000_000,
                    "DAYS" => 86_400_000_000_000,
                    _ => return None,
                })
            };
            let (to, from) = (nanos(&args[0])?, nanos(&args[2])?);
            let converted = (args[1].as_long() as i128 * from / to)
                .clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            handled(Value::Long(converted))
        }
        // Coroutine code boxes through `new Integer(i)`; boxed numbers are the
        // plain values here, so boxing returns its argument.
        "kotlin/coroutines/jvm/internal/Boxing" => match method_name {
            "boxInt" | "boxLong" | "boxDouble" | "boxFloat" | "boxBoolean" | "boxChar"
            | "boxShort" | "boxByte" => handled(args.first()?.clone()),
            _ => None,
        },
        // Boxed numbers are plain numeric values, so Number accessors convert them.
        "java/lang/Number" => match method_name {
            "intValue" => handled(Value::Int(args.first()?.as_int())),
            "longValue" => handled(Value::Long(args.first()?.as_long())),
            "doubleValue" => handled(Value::Double(args.first()?.as_double())),
            "floatValue" => handled(Value::Float(args.first()?.as_float())),
            "shortValue" => handled(Value::Int(args.first()?.as_int() as i16 as i32)),
            "byteValue" => handled(Value::Int(args.first()?.as_int() as i8 as i32)),
            _ => None,
        },
        "java/lang/Integer" => match method_name {
            "highestOneBit" => {
                let i = int(0)? as u32;
                handled(Value::Int(if i == 0 {
                    0
                } else {
                    (1u32 << (31 - i.leading_zeros())) as i32
                }))
            }
            "lowestOneBit" => {
                let i = int(0)?;
                handled(Value::Int(i & i.wrapping_neg()))
            }
            "numberOfLeadingZeros" => handled(Value::Int((int(0)? as u32).leading_zeros() as i32)),
            "numberOfTrailingZeros" => {
                handled(Value::Int((int(0)? as u32).trailing_zeros() as i32))
            }
            "bitCount" => handled(Value::Int((int(0)? as u32).count_ones() as i32)),
            "reverse" => handled(Value::Int((int(0)? as u32).reverse_bits() as i32)),
            "reverseBytes" => handled(Value::Int(int(0)?.swap_bytes())),
            "rotateLeft" => handled(Value::Int(int(0)?.rotate_left(int(1)? as u32 & 31))),
            "rotateRight" => handled(Value::Int(int(0)?.rotate_right(int(1)? as u32 & 31))),
            "signum" => handled(Value::Int(int(0)?.signum())),
            "compare" => handled(Value::Int(match int(0)?.cmp(&int(1)?) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })),
            "sum" => handled(Value::Int(int(0)?.wrapping_add(int(1)?))),
            "max" => handled(Value::Int(int(0)?.max(int(1)?))),
            "min" => handled(Value::Int(int(0)?.min(int(1)?))),
            "hashCode" if descriptor.starts_with("(I)") => handled(Value::Int(int(0)?)),
            "parseInt" | "valueOf"
                if args.first().is_some_and(|v| string_of(heap, v).is_some()) =>
            {
                let text = string_of(heap, &args[0])?;
                let radix = int(1).unwrap_or(10) as u32;
                // A malformed number would throw NumberFormatException; 0 is the quiet stand-in.
                handled(Value::Int(
                    i32::from_str_radix(text.trim(), radix).unwrap_or(0),
                ))
            }
            "toString"
                if descriptor.starts_with("(I)")
                    || descriptor.starts_with("(II)")
                    || descriptor == "()Ljava/lang/String;" =>
            {
                let radix = int(1).unwrap_or(10) as u32;
                let text = if radix == 10 {
                    int(0)?.to_string()
                } else {
                    radix_string(int(0)? as i64, radix)
                };
                let sid = heap.alloc_string(text);
                handled(Value::Object(Some(sid)))
            }
            "toHexString" => {
                let sid = heap.alloc_string(format!("{:x}", int(0)? as u32));
                handled(Value::Object(Some(sid)))
            }
            "toBinaryString" => {
                let sid = heap.alloc_string(format!("{:b}", int(0)? as u32));
                handled(Value::Object(Some(sid)))
            }
            _ => None,
        },
        "java/lang/Long" => match method_name {
            "numberOfLeadingZeros" => handled(Value::Int((long(0)? as u64).leading_zeros() as i32)),
            "numberOfTrailingZeros" => {
                handled(Value::Int((long(0)? as u64).trailing_zeros() as i32))
            }
            "bitCount" => handled(Value::Int((long(0)? as u64).count_ones() as i32)),
            "highestOneBit" => {
                let i = long(0)? as u64;
                handled(Value::Long(if i == 0 {
                    0
                } else {
                    (1u64 << (63 - i.leading_zeros())) as i64
                }))
            }
            "signum" => handled(Value::Int(long(0)?.signum() as i32)),
            "compare" => handled(Value::Int(match long(0)?.cmp(&long(1)?) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })),
            "hashCode" if descriptor.starts_with("(J)") => {
                let v = long(0)?;
                handled(Value::Int((v ^ (v >> 32)) as i32))
            }
            "parseLong" | "valueOf"
                if args.first().is_some_and(|v| string_of(heap, v).is_some()) =>
            {
                let text = string_of(heap, &args[0])?;
                handled(Value::Long(text.trim().parse().unwrap_or(0)))
            }
            "toString" if descriptor.starts_with("(J)") || descriptor == "()Ljava/lang/String;" => {
                let sid = heap.alloc_string(long(0)?.to_string());
                handled(Value::Object(Some(sid)))
            }
            _ => None,
        },
        "java/lang/Float" => match method_name {
            "parseFloat" | "valueOf"
                if args.first().is_some_and(|v| string_of(heap, v).is_some()) =>
            {
                let text = string_of(heap, &args[0])?;
                handled(Value::Float(text.trim().parse().unwrap_or(0.0)))
            }
            "compare" => handled(Value::Int(
                args.first()?
                    .as_float()
                    .partial_cmp(&args.get(1)?.as_float())
                    .map_or(0, |o| o as i32),
            )),
            "isNaN" if descriptor.starts_with("(F)") => {
                handled(Value::from_bool(args.first()?.as_float().is_nan()))
            }
            "floatToRawIntBits" => handled(Value::Int(args.first()?.as_float().to_bits() as i32)),
            // `floatToIntBits` collapses every NaN to the one canonical NaN.
            "floatToIntBits" => {
                let bits = args.first()?.as_float().to_bits();
                handled(Value::Int(if f32::from_bits(bits).is_nan() {
                    0x7fc0_0000
                } else {
                    bits as i32
                }))
            }
            "intBitsToFloat" => handled(Value::Float(f32::from_bits(int(0)? as u32))),
            _ => None,
        },
        "java/lang/Double" => match method_name {
            "parseDouble" | "valueOf"
                if args.first().is_some_and(|v| string_of(heap, v).is_some()) =>
            {
                let text = string_of(heap, &args[0])?;
                handled(Value::Double(text.trim().parse().unwrap_or(0.0)))
            }
            "compare" => handled(Value::Int(
                args.first()?
                    .as_double()
                    .partial_cmp(&args.get(1)?.as_double())
                    .map_or(0, |o| o as i32),
            )),
            "isNaN" if descriptor.starts_with("(D)") => {
                handled(Value::from_bool(args.first()?.as_double().is_nan()))
            }
            "doubleToRawLongBits" | "doubleToLongBits" => {
                handled(Value::Long(args.first()?.as_double().to_bits() as i64))
            }
            "longBitsToDouble" => handled(Value::Double(f64::from_bits(long(0)? as u64))),
            _ => None,
        },
        "java/lang/Character" => {
            let c = char::from_u32(int(0)? as u32);
            match method_name {
                // A boxed `Character` is the plain char value, as `Integer` is
                // the plain int, so boxing and unboxing return the char itself.
                "valueOf" | "charValue" => handled(Value::Int(int(0)?)),
                "isDigit" => handled(Value::from_bool(c.is_some_and(|c| c.is_numeric()))),
                "isLetter" => handled(Value::from_bool(c.is_some_and(|c| c.is_alphabetic()))),
                "isLetterOrDigit" => {
                    handled(Value::from_bool(c.is_some_and(|c| c.is_alphanumeric())))
                }
                "isWhitespace" | "isSpaceChar" => {
                    handled(Value::from_bool(c.is_some_and(|c| c.is_whitespace())))
                }
                "isUpperCase" => handled(Value::from_bool(c.is_some_and(|c| c.is_uppercase()))),
                "isLowerCase" => handled(Value::from_bool(c.is_some_and(|c| c.is_lowercase()))),
                "toLowerCase" => handled(Value::Int(
                    c.and_then(|c| c.to_lowercase().next())
                        .map_or(int(0)?, |c| c as i32),
                )),
                "toUpperCase" => handled(Value::Int(
                    c.and_then(|c| c.to_uppercase().next())
                        .map_or(int(0)?, |c| c as i32),
                )),
                "getNumericValue" => handled(Value::Int(
                    c.and_then(|c| c.to_digit(36)).map_or(-1, |d| d as i32),
                )),
                "digit" => handled(Value::Int(
                    c.and_then(|c| c.to_digit(int(1)? as u32))
                        .map_or(-1, |d| d as i32),
                )),
                "forDigit" => handled(Value::Int(
                    char::from_digit(int(0)? as u32, int(1)? as u32).map_or(0, |c| c as i32),
                )),
                "charCount" => handled(Value::Int(if int(0)? >= 0x10000 { 2 } else { 1 })),
                _ => None,
            }
        }
        _ => None,
    }
}

fn radix_string(mut value: i64, radix: u32) -> String {
    if value == 0 {
        return "0".into();
    }
    let negative = value < 0;
    let mut digits = Vec::new();
    while value != 0 {
        digits.push(
            char::from_digit((value % radix as i64).unsigned_abs() as u32, radix).unwrap_or('0'),
        );
        value /= radix as i64;
    }
    if negative {
        digits.push('-');
    }
    digits.iter().rev().collect()
}
