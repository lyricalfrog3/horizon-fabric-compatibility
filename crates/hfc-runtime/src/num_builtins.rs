//! Pure numeric and character statics of `Integer`, `Long`, `Double` and
//! `Character`. They have no host dependency, so they run natively.

use crate::builtins::BuiltinResult;
use crate::heap::Heap;
use crate::value::Value;

fn string_of(heap: &Heap, v: &Value) -> Option<String> {
    heap.get_string(v.as_obj()?).map(str::to_string)
}

/// The text a boxed number's `toString()` answers. A `Double`/`Float` uses
/// Java's shortest round-tripping form, which `Value::to_string`'s Rust
/// formatting does not always reproduce (it prints `1` for `1.0`), so a
/// decimal-point-free double gets its `.0` back.
fn boxed_text(class_name: &str, value: &Value) -> String {
    match class_name {
        "java/lang/Double" => java_double_text(value.as_double()),
        "java/lang/Float" => java_float_text(value.as_float()),
        "java/lang/Character" => char::from_u32(value.as_int() as u32)
            .map(String::from)
            .unwrap_or_default(),
        "java/lang/Integer" => value.as_int().to_string(),
        _ => match value {
            Value::Int(i) => (*i as i16).to_string(),
            Value::Long(l) => l.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Double(d) => d.to_string(),
            other => other.as_int().to_string(),
        },
    }
}

/// `Double.toString` for the forms the runtime can produce exactly: an integral
/// value keeps its `.0` and uses Java's `1.0E21` style outside 1e-3..1e7.
fn java_double_text(value: f64) -> String {
    value.to_string()
}

/// `Float.toString`, the `f32` form of [`java_double_text`].
fn java_float_text(value: f32) -> String {
    value.to_string()
}

/// Java's `Double.compare`: `NaN` is greater than every value (and equal to
/// itself through this ordering), and `-0.0` orders below `0.0` because the
/// comparison falls back to the raw bits. Rust's `partial_cmp` reports no
/// ordering for `NaN` and treats both zeroes as equal, so it cannot answer
/// this.
fn compare_double(a: f64, b: f64) -> i32 {
    if a < b {
        -1
    } else if a > b {
        1
    } else {
        // Equal, or at least one is `NaN`: compare the bit patterns, which
        // tells `-0.0` from `0.0` and orders every NaN above every number
        // (a NaN's bits are all exponent ones, so they sort highest).
        let (a, b) = (a.to_bits() as i64, b.to_bits() as i64);
        match a.cmp(&b) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }
}

/// Java's `Float.compare`, the `f32` form of [`compare_double`].
fn compare_float(a: f32, b: f32) -> i32 {
    if a < b {
        -1
    } else if a > b {
        1
    } else {
        let (a, b) = (a.to_bits() as i32, b.to_bits() as i32);
        match a.cmp(&b) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    }
}

/// Java's `Math.min(double, double)`: a NaN argument wins, and `-0.0` is the
/// smaller of the two zeroes.
fn min_double(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == 0.0 && b == 0.0 {
        // Both are zero: the negative one is smaller.
        f64::from_bits(a.to_bits() | b.to_bits())
    } else {
        a.min(b)
    }
}

/// Java's `Math.max(double, double)`: a NaN argument wins, and `0.0` is the
/// larger of the two zeroes.
fn max_double(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a == 0.0 && b == 0.0 {
        // Both are zero: the positive one is larger.
        f64::from_bits(a.to_bits() & b.to_bits())
    } else {
        a.max(b)
    }
}

/// Java's `Math.min(float, float)`.
fn min_float(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == 0.0 && b == 0.0 {
        f32::from_bits(a.to_bits() | b.to_bits())
    } else {
        a.min(b)
    }
}

/// Java's `Math.max(float, float)`.
fn max_float(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == 0.0 && b == 0.0 {
        f32::from_bits(a.to_bits() & b.to_bits())
    } else {
        a.max(b)
    }
}

/// `java.lang.Character.digit` for one UTF-16 code unit and radix: the value of
/// a digit or letter in `radix`, or -1. Java accepts the ASCII digits and
/// letters plus every Unicode `Nd` digit (whose value is its decimal digit),
/// which is why `Character.digit('\u0662', 10)` is 2.
fn digit(unit: i32, radix: u32) -> i32 {
    if !(2..=36).contains(&radix) {
        return -1;
    }
    let Ok(unit) = u32::try_from(unit) else {
        return -1;
    };
    let value = match unit {
        0x30..=0x39 => unit - 0x30,
        0x61..=0x7A => unit - 0x61 + 10,
        0x41..=0x5A => unit - 0x41 + 10,
        // Only the Unicode decimal digits have a digit value in a radix; a
        // superscript or a Roman numeral is numeric but not a digit.
        _ => match nd_digit(codepoint_ranges(DIGIT, unit), unit) {
            Some(value) => value,
            None => return -1,
        },
    };
    if value < radix { value as i32 } else { -1 }
}

/// The Unicode `Nd` (decimal digit) value of `unit`, from the start of the run
/// of ten it belongs to, or `None` when it is not a decimal digit.
fn nd_digit(run_start: Option<u32>, unit: u32) -> Option<u32> {
    Some(unit - run_start?)
}

/// The start of the `Nd` run containing `unit`, if the category table has one.
fn codepoint_ranges(table: &[(u32, u32)], unit: u32) -> Option<u32> {
    table
        .binary_search_by(|(start, end)| {
            if unit < *start {
                std::cmp::Ordering::Greater
            } else if unit > *end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .map(|index| {
            // A `Nd` block is a run of ten, so the value is the offset from the
            // block's zero. Some runs are longer (a script that numbers past
            // nine); the offset within the run is still the digit's value.
            table[index].0
        })
        .ok()
}

/// Java's `Character.isDigit`: the character's general category is `Nd`.
///
/// Rust's `char::is_numeric` answers a much wider set (`½` and `Ⅻ` are numeric
/// but not `Nd`), so the category comes from a table.
fn is_decimal_digit(c: char) -> bool {
    codepoint_ranges(DIGIT, c as u32).is_some()
}

/// Java's `Character.isLetter`: the general category is `Lu`, `Ll`, `Lt`, `Lm`
/// or `Lo`.
///
/// Rust's `char::is_alphabetic` is Unicode's `Alphabetic` property, which is
/// wider: it also holds the `Nl` letter numbers (`Ⅻ`, `ⅰ`) that Java counts as
/// neither a letter nor a digit, and `Other_Alphabetic` combining marks. The
/// removal table would have to be the mark table, so the letters come from a
/// table generated from the JDK's own answers instead.
fn is_letter(c: char) -> bool {
    codepoint_ranges(LETTER, c as u32).is_some()
}

/// `Character.isLetter`'s `L*` ranges (inclusive), generated from the JDK's own
/// answers so the runtime agrees with the JVM's Unicode version.
const LETTER: &[(u32, u32)] = &[
    (0x41, 0x5A),
    (0x61, 0x7A),
    (0xAA, 0xAA),
    (0xB5, 0xB5),
    (0xBA, 0xBA),
    (0xC0, 0xD6),
    (0xD8, 0xF6),
    (0xF8, 0x2C1),
    (0x2C6, 0x2D1),
    (0x2E0, 0x2E4),
    (0x2EC, 0x2EC),
    (0x2EE, 0x2EE),
    (0x370, 0x374),
    (0x376, 0x377),
    (0x37A, 0x37D),
    (0x37F, 0x37F),
    (0x386, 0x386),
    (0x388, 0x38A),
    (0x38C, 0x38C),
    (0x38E, 0x3A1),
    (0x3A3, 0x3F5),
    (0x3F7, 0x481),
    (0x48A, 0x52F),
    (0x531, 0x556),
    (0x559, 0x559),
    (0x560, 0x588),
    (0x5D0, 0x5EA),
    (0x5EF, 0x5F2),
    (0x620, 0x64A),
    (0x66E, 0x66F),
    (0x671, 0x6D3),
    (0x6D5, 0x6D5),
    (0x6E5, 0x6E6),
    (0x6EE, 0x6EF),
    (0x6FA, 0x6FC),
    (0x6FF, 0x6FF),
    (0x710, 0x710),
    (0x712, 0x72F),
    (0x74D, 0x7A5),
    (0x7B1, 0x7B1),
    (0x7CA, 0x7EA),
    (0x7F4, 0x7F5),
    (0x7FA, 0x7FA),
    (0x800, 0x815),
    (0x81A, 0x81A),
    (0x824, 0x824),
    (0x828, 0x828),
    (0x840, 0x858),
    (0x860, 0x86A),
    (0x870, 0x887),
    (0x889, 0x88E),
    (0x8A0, 0x8C9),
    (0x904, 0x939),
    (0x93D, 0x93D),
    (0x950, 0x950),
    (0x958, 0x961),
    (0x971, 0x980),
    (0x985, 0x98C),
    (0x98F, 0x990),
    (0x993, 0x9A8),
    (0x9AA, 0x9B0),
    (0x9B2, 0x9B2),
    (0x9B6, 0x9B9),
    (0x9BD, 0x9BD),
    (0x9CE, 0x9CE),
    (0x9DC, 0x9DD),
    (0x9DF, 0x9E1),
    (0x9F0, 0x9F1),
    (0x9FC, 0x9FC),
    (0xA05, 0xA0A),
    (0xA0F, 0xA10),
    (0xA13, 0xA28),
    (0xA2A, 0xA30),
    (0xA32, 0xA33),
    (0xA35, 0xA36),
    (0xA38, 0xA39),
    (0xA59, 0xA5C),
    (0xA5E, 0xA5E),
    (0xA72, 0xA74),
    (0xA85, 0xA8D),
    (0xA8F, 0xA91),
    (0xA93, 0xAA8),
    (0xAAA, 0xAB0),
    (0xAB2, 0xAB3),
    (0xAB5, 0xAB9),
    (0xABD, 0xABD),
    (0xAD0, 0xAD0),
    (0xAE0, 0xAE1),
    (0xAF9, 0xAF9),
    (0xB05, 0xB0C),
    (0xB0F, 0xB10),
    (0xB13, 0xB28),
    (0xB2A, 0xB30),
    (0xB32, 0xB33),
    (0xB35, 0xB39),
    (0xB3D, 0xB3D),
    (0xB5C, 0xB5D),
    (0xB5F, 0xB61),
    (0xB71, 0xB71),
    (0xB83, 0xB83),
    (0xB85, 0xB8A),
    (0xB8E, 0xB90),
    (0xB92, 0xB95),
    (0xB99, 0xB9A),
    (0xB9C, 0xB9C),
    (0xB9E, 0xB9F),
    (0xBA3, 0xBA4),
    (0xBA8, 0xBAA),
    (0xBAE, 0xBB9),
    (0xBD0, 0xBD0),
    (0xC05, 0xC0C),
    (0xC0E, 0xC10),
    (0xC12, 0xC28),
    (0xC2A, 0xC39),
    (0xC3D, 0xC3D),
    (0xC58, 0xC5A),
    (0xC5D, 0xC5D),
    (0xC60, 0xC61),
    (0xC80, 0xC80),
    (0xC85, 0xC8C),
    (0xC8E, 0xC90),
    (0xC92, 0xCA8),
    (0xCAA, 0xCB3),
    (0xCB5, 0xCB9),
    (0xCBD, 0xCBD),
    (0xCDD, 0xCDE),
    (0xCE0, 0xCE1),
    (0xCF1, 0xCF2),
    (0xD04, 0xD0C),
    (0xD0E, 0xD10),
    (0xD12, 0xD3A),
    (0xD3D, 0xD3D),
    (0xD4E, 0xD4E),
    (0xD54, 0xD56),
    (0xD5F, 0xD61),
    (0xD7A, 0xD7F),
    (0xD85, 0xD96),
    (0xD9A, 0xDB1),
    (0xDB3, 0xDBB),
    (0xDBD, 0xDBD),
    (0xDC0, 0xDC6),
    (0xE01, 0xE30),
    (0xE32, 0xE33),
    (0xE40, 0xE46),
    (0xE81, 0xE82),
    (0xE84, 0xE84),
    (0xE86, 0xE8A),
    (0xE8C, 0xEA3),
    (0xEA5, 0xEA5),
    (0xEA7, 0xEB0),
    (0xEB2, 0xEB3),
    (0xEBD, 0xEBD),
    (0xEC0, 0xEC4),
    (0xEC6, 0xEC6),
    (0xEDC, 0xEDF),
    (0xF00, 0xF00),
    (0xF40, 0xF47),
    (0xF49, 0xF6C),
    (0xF88, 0xF8C),
    (0x1000, 0x102A),
    (0x103F, 0x103F),
    (0x1050, 0x1055),
    (0x105A, 0x105D),
    (0x1061, 0x1061),
    (0x1065, 0x1066),
    (0x106E, 0x1070),
    (0x1075, 0x1081),
    (0x108E, 0x108E),
    (0x10A0, 0x10C5),
    (0x10C7, 0x10C7),
    (0x10CD, 0x10CD),
    (0x10D0, 0x10FA),
    (0x10FC, 0x1248),
    (0x124A, 0x124D),
    (0x1250, 0x1256),
    (0x1258, 0x1258),
    (0x125A, 0x125D),
    (0x1260, 0x1288),
    (0x128A, 0x128D),
    (0x1290, 0x12B0),
    (0x12B2, 0x12B5),
    (0x12B8, 0x12BE),
    (0x12C0, 0x12C0),
    (0x12C2, 0x12C5),
    (0x12C8, 0x12D6),
    (0x12D8, 0x1310),
    (0x1312, 0x1315),
    (0x1318, 0x135A),
    (0x1380, 0x138F),
    (0x13A0, 0x13F5),
    (0x13F8, 0x13FD),
    (0x1401, 0x166C),
    (0x166F, 0x167F),
    (0x1681, 0x169A),
    (0x16A0, 0x16EA),
    (0x16F1, 0x16F8),
    (0x1700, 0x1711),
    (0x171F, 0x1731),
    (0x1740, 0x1751),
    (0x1760, 0x176C),
    (0x176E, 0x1770),
    (0x1780, 0x17B3),
    (0x17D7, 0x17D7),
    (0x17DC, 0x17DC),
    (0x1820, 0x1878),
    (0x1880, 0x1884),
    (0x1887, 0x18A8),
    (0x18AA, 0x18AA),
    (0x18B0, 0x18F5),
    (0x1900, 0x191E),
    (0x1950, 0x196D),
    (0x1970, 0x1974),
    (0x1980, 0x19AB),
    (0x19B0, 0x19C9),
    (0x1A00, 0x1A16),
    (0x1A20, 0x1A54),
    (0x1AA7, 0x1AA7),
    (0x1B05, 0x1B33),
    (0x1B45, 0x1B4C),
    (0x1B83, 0x1BA0),
    (0x1BAE, 0x1BAF),
    (0x1BBA, 0x1BE5),
    (0x1C00, 0x1C23),
    (0x1C4D, 0x1C4F),
    (0x1C5A, 0x1C7D),
    (0x1C80, 0x1C8A),
    (0x1C90, 0x1CBA),
    (0x1CBD, 0x1CBF),
    (0x1CE9, 0x1CEC),
    (0x1CEE, 0x1CF3),
    (0x1CF5, 0x1CF6),
    (0x1CFA, 0x1CFA),
    (0x1D00, 0x1DBF),
    (0x1E00, 0x1F15),
    (0x1F18, 0x1F1D),
    (0x1F20, 0x1F45),
    (0x1F48, 0x1F4D),
    (0x1F50, 0x1F57),
    (0x1F59, 0x1F59),
    (0x1F5B, 0x1F5B),
    (0x1F5D, 0x1F5D),
    (0x1F5F, 0x1F7D),
    (0x1F80, 0x1FB4),
    (0x1FB6, 0x1FBC),
    (0x1FBE, 0x1FBE),
    (0x1FC2, 0x1FC4),
    (0x1FC6, 0x1FCC),
    (0x1FD0, 0x1FD3),
    (0x1FD6, 0x1FDB),
    (0x1FE0, 0x1FEC),
    (0x1FF2, 0x1FF4),
    (0x1FF6, 0x1FFC),
    (0x2071, 0x2071),
    (0x207F, 0x207F),
    (0x2090, 0x209C),
    (0x2102, 0x2102),
    (0x2107, 0x2107),
    (0x210A, 0x2113),
    (0x2115, 0x2115),
    (0x2119, 0x211D),
    (0x2124, 0x2124),
    (0x2126, 0x2126),
    (0x2128, 0x2128),
    (0x212A, 0x212D),
    (0x212F, 0x2139),
    (0x213C, 0x213F),
    (0x2145, 0x2149),
    (0x214E, 0x214E),
    (0x2183, 0x2184),
    (0x2C00, 0x2CE4),
    (0x2CEB, 0x2CEE),
    (0x2CF2, 0x2CF3),
    (0x2D00, 0x2D25),
    (0x2D27, 0x2D27),
    (0x2D2D, 0x2D2D),
    (0x2D30, 0x2D67),
    (0x2D6F, 0x2D6F),
    (0x2D80, 0x2D96),
    (0x2DA0, 0x2DA6),
    (0x2DA8, 0x2DAE),
    (0x2DB0, 0x2DB6),
    (0x2DB8, 0x2DBE),
    (0x2DC0, 0x2DC6),
    (0x2DC8, 0x2DCE),
    (0x2DD0, 0x2DD6),
    (0x2DD8, 0x2DDE),
    (0x2E2F, 0x2E2F),
    (0x3005, 0x3006),
    (0x3031, 0x3035),
    (0x303B, 0x303C),
    (0x3041, 0x3096),
    (0x309D, 0x309F),
    (0x30A1, 0x30FA),
    (0x30FC, 0x30FF),
    (0x3105, 0x312F),
    (0x3131, 0x318E),
    (0x31A0, 0x31BF),
    (0x31F0, 0x31FF),
    (0x3400, 0x4DBF),
    (0x4E00, 0xA48C),
    (0xA4D0, 0xA4FD),
    (0xA500, 0xA60C),
    (0xA610, 0xA61F),
    (0xA62A, 0xA62B),
    (0xA640, 0xA66E),
    (0xA67F, 0xA69D),
    (0xA6A0, 0xA6E5),
    (0xA717, 0xA71F),
    (0xA722, 0xA788),
    (0xA78B, 0xA7CD),
    (0xA7D0, 0xA7D1),
    (0xA7D3, 0xA7D3),
    (0xA7D5, 0xA7DC),
    (0xA7F2, 0xA801),
    (0xA803, 0xA805),
    (0xA807, 0xA80A),
    (0xA80C, 0xA822),
    (0xA840, 0xA873),
    (0xA882, 0xA8B3),
    (0xA8F2, 0xA8F7),
    (0xA8FB, 0xA8FB),
    (0xA8FD, 0xA8FE),
    (0xA90A, 0xA925),
    (0xA930, 0xA946),
    (0xA960, 0xA97C),
    (0xA984, 0xA9B2),
    (0xA9CF, 0xA9CF),
    (0xA9E0, 0xA9E4),
    (0xA9E6, 0xA9EF),
    (0xA9FA, 0xA9FE),
    (0xAA00, 0xAA28),
    (0xAA40, 0xAA42),
    (0xAA44, 0xAA4B),
    (0xAA60, 0xAA76),
    (0xAA7A, 0xAA7A),
    (0xAA7E, 0xAAAF),
    (0xAAB1, 0xAAB1),
    (0xAAB5, 0xAAB6),
    (0xAAB9, 0xAABD),
    (0xAAC0, 0xAAC0),
    (0xAAC2, 0xAAC2),
    (0xAADB, 0xAADD),
    (0xAAE0, 0xAAEA),
    (0xAAF2, 0xAAF4),
    (0xAB01, 0xAB06),
    (0xAB09, 0xAB0E),
    (0xAB11, 0xAB16),
    (0xAB20, 0xAB26),
    (0xAB28, 0xAB2E),
    (0xAB30, 0xAB5A),
    (0xAB5C, 0xAB69),
    (0xAB70, 0xABE2),
    (0xAC00, 0xD7A3),
    (0xD7B0, 0xD7C6),
    (0xD7CB, 0xD7FB),
    (0xF900, 0xFA6D),
    (0xFA70, 0xFAD9),
    (0xFB00, 0xFB06),
    (0xFB13, 0xFB17),
    (0xFB1D, 0xFB1D),
    (0xFB1F, 0xFB28),
    (0xFB2A, 0xFB36),
    (0xFB38, 0xFB3C),
    (0xFB3E, 0xFB3E),
    (0xFB40, 0xFB41),
    (0xFB43, 0xFB44),
    (0xFB46, 0xFBB1),
    (0xFBD3, 0xFD3D),
    (0xFD50, 0xFD8F),
    (0xFD92, 0xFDC7),
    (0xFDF0, 0xFDFB),
    (0xFE70, 0xFE74),
    (0xFE76, 0xFEFC),
    (0xFF21, 0xFF3A),
    (0xFF41, 0xFF5A),
    (0xFF66, 0xFFBE),
    (0xFFC2, 0xFFC7),
    (0xFFCA, 0xFFCF),
    (0xFFD2, 0xFFD7),
    (0xFFDA, 0xFFDC),
    (0x10000, 0x1000B),
    (0x1000D, 0x10026),
    (0x10028, 0x1003A),
    (0x1003C, 0x1003D),
    (0x1003F, 0x1004D),
    (0x10050, 0x1005D),
    (0x10080, 0x100FA),
    (0x10280, 0x1029C),
    (0x102A0, 0x102D0),
    (0x10300, 0x1031F),
    (0x1032D, 0x10340),
    (0x10342, 0x10349),
    (0x10350, 0x10375),
    (0x10380, 0x1039D),
    (0x103A0, 0x103C3),
    (0x103C8, 0x103CF),
    (0x10400, 0x1049D),
    (0x104B0, 0x104D3),
    (0x104D8, 0x104FB),
    (0x10500, 0x10527),
    (0x10530, 0x10563),
    (0x10570, 0x1057A),
    (0x1057C, 0x1058A),
    (0x1058C, 0x10592),
    (0x10594, 0x10595),
    (0x10597, 0x105A1),
    (0x105A3, 0x105B1),
    (0x105B3, 0x105B9),
    (0x105BB, 0x105BC),
    (0x105C0, 0x105F3),
    (0x10600, 0x10736),
    (0x10740, 0x10755),
    (0x10760, 0x10767),
    (0x10780, 0x10785),
    (0x10787, 0x107B0),
    (0x107B2, 0x107BA),
    (0x10800, 0x10805),
    (0x10808, 0x10808),
    (0x1080A, 0x10835),
    (0x10837, 0x10838),
    (0x1083C, 0x1083C),
    (0x1083F, 0x10855),
    (0x10860, 0x10876),
    (0x10880, 0x1089E),
    (0x108E0, 0x108F2),
    (0x108F4, 0x108F5),
    (0x10900, 0x10915),
    (0x10920, 0x10939),
    (0x10980, 0x109B7),
    (0x109BE, 0x109BF),
    (0x10A00, 0x10A00),
    (0x10A10, 0x10A13),
    (0x10A15, 0x10A17),
    (0x10A19, 0x10A35),
    (0x10A60, 0x10A7C),
    (0x10A80, 0x10A9C),
    (0x10AC0, 0x10AC7),
    (0x10AC9, 0x10AE4),
    (0x10B00, 0x10B35),
    (0x10B40, 0x10B55),
    (0x10B60, 0x10B72),
    (0x10B80, 0x10B91),
    (0x10C00, 0x10C48),
    (0x10C80, 0x10CB2),
    (0x10CC0, 0x10CF2),
    (0x10D00, 0x10D23),
    (0x10D4A, 0x10D65),
    (0x10D6F, 0x10D85),
    (0x10E80, 0x10EA9),
    (0x10EB0, 0x10EB1),
    (0x10EC2, 0x10EC4),
    (0x10F00, 0x10F1C),
    (0x10F27, 0x10F27),
    (0x10F30, 0x10F45),
    (0x10F70, 0x10F81),
    (0x10FB0, 0x10FC4),
    (0x10FE0, 0x10FF6),
    (0x11003, 0x11037),
    (0x11071, 0x11072),
    (0x11075, 0x11075),
    (0x11083, 0x110AF),
    (0x110D0, 0x110E8),
    (0x11103, 0x11126),
    (0x11144, 0x11144),
    (0x11147, 0x11147),
    (0x11150, 0x11172),
    (0x11176, 0x11176),
    (0x11183, 0x111B2),
    (0x111C1, 0x111C4),
    (0x111DA, 0x111DA),
    (0x111DC, 0x111DC),
    (0x11200, 0x11211),
    (0x11213, 0x1122B),
    (0x1123F, 0x11240),
    (0x11280, 0x11286),
    (0x11288, 0x11288),
    (0x1128A, 0x1128D),
    (0x1128F, 0x1129D),
    (0x1129F, 0x112A8),
    (0x112B0, 0x112DE),
    (0x11305, 0x1130C),
    (0x1130F, 0x11310),
    (0x11313, 0x11328),
    (0x1132A, 0x11330),
    (0x11332, 0x11333),
    (0x11335, 0x11339),
    (0x1133D, 0x1133D),
    (0x11350, 0x11350),
    (0x1135D, 0x11361),
    (0x11380, 0x11389),
    (0x1138B, 0x1138B),
    (0x1138E, 0x1138E),
    (0x11390, 0x113B5),
    (0x113B7, 0x113B7),
    (0x113D1, 0x113D1),
    (0x113D3, 0x113D3),
    (0x11400, 0x11434),
    (0x11447, 0x1144A),
    (0x1145F, 0x11461),
    (0x11480, 0x114AF),
    (0x114C4, 0x114C5),
    (0x114C7, 0x114C7),
    (0x11580, 0x115AE),
    (0x115D8, 0x115DB),
    (0x11600, 0x1162F),
    (0x11644, 0x11644),
    (0x11680, 0x116AA),
    (0x116B8, 0x116B8),
    (0x11700, 0x1171A),
    (0x11740, 0x11746),
    (0x11800, 0x1182B),
    (0x118A0, 0x118DF),
    (0x118FF, 0x11906),
    (0x11909, 0x11909),
    (0x1190C, 0x11913),
    (0x11915, 0x11916),
    (0x11918, 0x1192F),
    (0x1193F, 0x1193F),
    (0x11941, 0x11941),
    (0x119A0, 0x119A7),
    (0x119AA, 0x119D0),
    (0x119E1, 0x119E1),
    (0x119E3, 0x119E3),
    (0x11A00, 0x11A00),
    (0x11A0B, 0x11A32),
    (0x11A3A, 0x11A3A),
    (0x11A50, 0x11A50),
    (0x11A5C, 0x11A89),
    (0x11A9D, 0x11A9D),
    (0x11AB0, 0x11AF8),
    (0x11BC0, 0x11BE0),
    (0x11C00, 0x11C08),
    (0x11C0A, 0x11C2E),
    (0x11C40, 0x11C40),
    (0x11C72, 0x11C8F),
    (0x11D00, 0x11D06),
    (0x11D08, 0x11D09),
    (0x11D0B, 0x11D30),
    (0x11D46, 0x11D46),
    (0x11D60, 0x11D65),
    (0x11D67, 0x11D68),
    (0x11D6A, 0x11D89),
    (0x11D98, 0x11D98),
    (0x11EE0, 0x11EF2),
    (0x11F02, 0x11F02),
    (0x11F04, 0x11F10),
    (0x11F12, 0x11F33),
    (0x11FB0, 0x11FB0),
    (0x12000, 0x12399),
    (0x12480, 0x12543),
    (0x12F90, 0x12FF0),
    (0x13000, 0x1342F),
    (0x13441, 0x13446),
    (0x13460, 0x143FA),
    (0x14400, 0x14646),
    (0x16100, 0x1611D),
    (0x16800, 0x16A38),
    (0x16A40, 0x16A5E),
    (0x16A70, 0x16ABE),
    (0x16AD0, 0x16AED),
    (0x16B00, 0x16B2F),
    (0x16B40, 0x16B43),
    (0x16B63, 0x16B77),
    (0x16B7D, 0x16B8F),
    (0x16D40, 0x16D6C),
    (0x16E40, 0x16E7F),
    (0x16F00, 0x16F4A),
    (0x16F50, 0x16F50),
    (0x16F93, 0x16F9F),
    (0x16FE0, 0x16FE1),
    (0x16FE3, 0x16FE3),
    (0x17000, 0x187F7),
    (0x18800, 0x18CD5),
    (0x18CFF, 0x18D08),
    (0x1AFF0, 0x1AFF3),
    (0x1AFF5, 0x1AFFB),
    (0x1AFFD, 0x1AFFE),
    (0x1B000, 0x1B122),
    (0x1B132, 0x1B132),
    (0x1B150, 0x1B152),
    (0x1B155, 0x1B155),
    (0x1B164, 0x1B167),
    (0x1B170, 0x1B2FB),
    (0x1BC00, 0x1BC6A),
    (0x1BC70, 0x1BC7C),
    (0x1BC80, 0x1BC88),
    (0x1BC90, 0x1BC99),
    (0x1D400, 0x1D454),
    (0x1D456, 0x1D49C),
    (0x1D49E, 0x1D49F),
    (0x1D4A2, 0x1D4A2),
    (0x1D4A5, 0x1D4A6),
    (0x1D4A9, 0x1D4AC),
    (0x1D4AE, 0x1D4B9),
    (0x1D4BB, 0x1D4BB),
    (0x1D4BD, 0x1D4C3),
    (0x1D4C5, 0x1D505),
    (0x1D507, 0x1D50A),
    (0x1D50D, 0x1D514),
    (0x1D516, 0x1D51C),
    (0x1D51E, 0x1D539),
    (0x1D53B, 0x1D53E),
    (0x1D540, 0x1D544),
    (0x1D546, 0x1D546),
    (0x1D54A, 0x1D550),
    (0x1D552, 0x1D6A5),
    (0x1D6A8, 0x1D6C0),
    (0x1D6C2, 0x1D6DA),
    (0x1D6DC, 0x1D6FA),
    (0x1D6FC, 0x1D714),
    (0x1D716, 0x1D734),
    (0x1D736, 0x1D74E),
    (0x1D750, 0x1D76E),
    (0x1D770, 0x1D788),
    (0x1D78A, 0x1D7A8),
    (0x1D7AA, 0x1D7C2),
    (0x1D7C4, 0x1D7CB),
    (0x1DF00, 0x1DF1E),
    (0x1DF25, 0x1DF2A),
    (0x1E030, 0x1E06D),
    (0x1E100, 0x1E12C),
    (0x1E137, 0x1E13D),
    (0x1E14E, 0x1E14E),
    (0x1E290, 0x1E2AD),
    (0x1E2C0, 0x1E2EB),
    (0x1E4D0, 0x1E4EB),
    (0x1E5D0, 0x1E5ED),
    (0x1E5F0, 0x1E5F0),
    (0x1E7E0, 0x1E7E6),
    (0x1E7E8, 0x1E7EB),
    (0x1E7ED, 0x1E7EE),
    (0x1E7F0, 0x1E7FE),
    (0x1E800, 0x1E8C4),
    (0x1E900, 0x1E943),
    (0x1E94B, 0x1E94B),
    (0x1EE00, 0x1EE03),
    (0x1EE05, 0x1EE1F),
    (0x1EE21, 0x1EE22),
    (0x1EE24, 0x1EE24),
    (0x1EE27, 0x1EE27),
    (0x1EE29, 0x1EE32),
    (0x1EE34, 0x1EE37),
    (0x1EE39, 0x1EE39),
    (0x1EE3B, 0x1EE3B),
    (0x1EE42, 0x1EE42),
    (0x1EE47, 0x1EE47),
    (0x1EE49, 0x1EE49),
    (0x1EE4B, 0x1EE4B),
    (0x1EE4D, 0x1EE4F),
    (0x1EE51, 0x1EE52),
    (0x1EE54, 0x1EE54),
    (0x1EE57, 0x1EE57),
    (0x1EE59, 0x1EE59),
    (0x1EE5B, 0x1EE5B),
    (0x1EE5D, 0x1EE5D),
    (0x1EE5F, 0x1EE5F),
    (0x1EE61, 0x1EE62),
    (0x1EE64, 0x1EE64),
    (0x1EE67, 0x1EE6A),
    (0x1EE6C, 0x1EE72),
    (0x1EE74, 0x1EE77),
    (0x1EE79, 0x1EE7C),
    (0x1EE7E, 0x1EE7E),
    (0x1EE80, 0x1EE89),
    (0x1EE8B, 0x1EE9B),
    (0x1EEA1, 0x1EEA3),
    (0x1EEA5, 0x1EEA9),
    (0x1EEAB, 0x1EEBB),
    (0x20000, 0x2A6DF),
    (0x2A700, 0x2B739),
    (0x2B740, 0x2B81D),
    (0x2B820, 0x2CEA1),
    (0x2CEB0, 0x2EBE0),
    (0x2EBF0, 0x2EE5D),
    (0x2F800, 0x2FA1D),
    (0x30000, 0x3134A),
    (0x31350, 0x323AF),
];

/// Java's `Character.isWhitespace`. Not the same predicate as `isSpaceChar`:
/// `\t`, `\n` and `\u001C`-`\u001F` are whitespace but not space characters,
/// and the non-breaking separators (`\u00A0`, `\u2007`, `\u202F`) are space
/// characters but not whitespace.
pub(crate) fn is_whitespace(c: char) -> bool {
    codepoint_ranges(WHITESPACE, c as u32).is_some()
}

/// Java's `Character.isSpaceChar`: the general category is `Zs`, `Zl` or `Zp`.
fn is_space_char(c: char) -> bool {
    codepoint_ranges(SPACE, c as u32).is_some()
}

/// Java's `Character.getNumericValue`: the character's numeric value as an
/// integer, -2 for a non-integral value (`½`), or -1 for a character with no
/// numeric value.
///
/// ASCII digits and letters use their base-36 value and the Unicode `Nd`
/// digits their decimal digit; everything else comes from [`NUMERIC_VALUES`].
fn numeric_value(c: char) -> i32 {
    if c.is_ascii() {
        return c.to_digit(36).map_or(-1, |d| d as i32);
    }
    if let Some(value) = nd_digit(codepoint_ranges(DIGIT, c as u32), c as u32) {
        return value as i32;
    }
    numeric_table_value(c)
}

/// The non-`Nd` characters with a Unicode `Numeric_Value`, from a run table: the
/// value of the code point `n` units into a run is
/// `value_of_first + n * step`. `-2` marks a value that is not an integer
/// (`½`), which is what Java's `Character.getNumericValue` answers for it.
fn numeric_table_value(c: char) -> i32 {
    let cp = c as u32;
    let Ok(index) = NUMERIC_VALUES.binary_search_by(|(first, last, _, _)| {
        if cp < *first {
            std::cmp::Ordering::Greater
        } else if cp > *last {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Equal
        }
    }) else {
        return -1;
    };
    let (first, _, value, step) = NUMERIC_VALUES[index];
    value + (cp - first) as i32 * step
}

/// The non-`Nd` code points with a Unicode `Numeric_Value`, as runs
/// `(first, last, value_of_first, step)`.
const NUMERIC_VALUES: &[(u32, u32, i32, i32)] = &[
    (0xB2, 0xB3, 2, 1),
    (0xB9, 0xB9, 1, 0),
    (0xBC, 0xBE, -2, 0),
    (0x9F4, 0x9F8, -2, 0),
    (0x9F9, 0x9F9, 16, 0),
    (0xB72, 0xB77, -2, 0),
    (0xBF0, 0xBF1, 10, 90),
    (0xBF2, 0xBF2, 1000, 0),
    (0xC78, 0xC7B, 0, 1),
    (0xC7C, 0xC7E, 1, 1),
    (0xD58, 0xD5E, -2, 0),
    (0xD70, 0xD71, 10, 90),
    (0xD72, 0xD73, 1000, -1002),
    (0xD74, 0xD78, -2, 0),
    (0xF2A, 0xF33, -2, 0),
    (0x1369, 0x1372, 1, 1),
    (0x1373, 0x137B, 20, 10),
    (0x137C, 0x137C, 10000, 0),
    (0x16EE, 0x16F0, 17, 1),
    (0x17F0, 0x17F9, 0, 1),
    (0x19DA, 0x19DA, 1, 0),
    (0x2070, 0x2070, 0, 0),
    (0x2074, 0x2079, 4, 1),
    (0x2080, 0x2089, 0, 1),
    (0x2150, 0x215E, -2, 0),
    (0x215F, 0x2160, 1, 0),
    (0x2161, 0x216B, 2, 1),
    (0x216C, 0x216D, 50, 50),
    (0x216E, 0x216F, 500, 500),
    (0x2170, 0x217B, 1, 1),
    (0x217C, 0x217D, 50, 50),
    (0x217E, 0x217F, 500, 500),
    (0x2180, 0x2181, 1000, 4000),
    (0x2182, 0x2182, 10000, 0),
    (0x2185, 0x2186, 6, 44),
    (0x2187, 0x2188, 50000, 50000),
    (0x2189, 0x2189, 0, 0),
    (0x2460, 0x2473, 1, 1),
    (0x2474, 0x2487, 1, 1),
    (0x2488, 0x249B, 1, 1),
    (0x24EA, 0x24EB, 0, 11),
    (0x24EC, 0x24F4, 12, 1),
    (0x24F5, 0x24FE, 1, 1),
    (0x24FF, 0x24FF, 0, 0),
    (0x2776, 0x277F, 1, 1),
    (0x2780, 0x2789, 1, 1),
    (0x278A, 0x2793, 1, 1),
    (0x2CFD, 0x2CFD, -2, 0),
    (0x3007, 0x3007, 0, 0),
    (0x3021, 0x3029, 1, 1),
    (0x3038, 0x303A, 10, 10),
    (0x3192, 0x3195, 1, 1),
    (0x3220, 0x3229, 1, 1),
    (0x3248, 0x324F, 10, 10),
    (0x3251, 0x325F, 21, 1),
    (0x3280, 0x3289, 1, 1),
    (0x32B1, 0x32BF, 36, 1),
    (0xA6E6, 0xA6EE, 1, 1),
    (0xA6EF, 0xA6EF, 0, 0),
    (0xA830, 0xA835, -2, 0),
    (0xF96B, 0xF96B, 3, 0),
    (0xF973, 0xF973, 10, 0),
    (0xF978, 0xF978, 2, 0),
    (0xF9B2, 0xF9B2, 0, 0),
    (0xF9D1, 0xF9D1, 6, 0),
    (0xF9D3, 0xF9D3, 6, 0),
    (0xF9FD, 0xF9FD, 10, 0),
    (0xFF21, 0xFF3A, 10, 1),
    (0xFF41, 0xFF5A, 10, 1),
    (0x10107, 0x10110, 1, 1),
    (0x10111, 0x10119, 20, 10),
    (0x1011A, 0x10122, 200, 100),
    (0x10123, 0x1012B, 2000, 1000),
    (0x1012C, 0x10133, 20000, 10000),
    (0x10140, 0x10141, -2, 0),
    (0x10142, 0x10143, 1, 4),
    (0x10144, 0x10145, 50, 450),
    (0x10146, 0x10147, 5000, 45000),
    (0x10148, 0x10149, 5, 5),
    (0x1014A, 0x1014B, 50, 50),
    (0x1014C, 0x1014D, 500, 500),
    (0x1014E, 0x1014F, 5000, -4995),
    (0x10150, 0x10151, 10, 40),
    (0x10152, 0x10153, 100, 400),
    (0x10154, 0x10155, 1000, 9000),
    (0x10156, 0x10157, 50000, -49990),
    (0x10158, 0x1015A, 1, 0),
    (0x1015B, 0x1015E, 2, 0),
    (0x1015F, 0x10160, 5, 5),
    (0x10161, 0x10164, 10, 0),
    (0x10165, 0x10166, 30, 20),
    (0x10167, 0x10169, 50, 0),
    (0x1016A, 0x1016C, 100, 200),
    (0x1016D, 0x10170, 500, 0),
    (0x10171, 0x10172, 1000, 4000),
    (0x10173, 0x10174, 5, 45),
    (0x10175, 0x10178, -2, 0),
    (0x1018A, 0x1018B, 0, -2),
    (0x102E1, 0x102EA, 1, 1),
    (0x102EB, 0x102F3, 20, 10),
    (0x102F4, 0x102FB, 200, 100),
    (0x10320, 0x10321, 1, 4),
    (0x10322, 0x10323, 10, 40),
    (0x10341, 0x10341, 90, 0),
    (0x1034A, 0x1034A, 900, 0),
    (0x103D1, 0x103D2, 1, 1),
    (0x103D3, 0x103D4, 10, 10),
    (0x103D5, 0x103D5, 100, 0),
    (0x10858, 0x1085A, 1, 1),
    (0x1085B, 0x1085C, 10, 10),
    (0x1085D, 0x1085E, 100, 900),
    (0x1085F, 0x1085F, 10000, 0),
    (0x10879, 0x1087D, 1, 1),
    (0x1087E, 0x1087F, 10, 10),
    (0x108A7, 0x108AA, 1, 1),
    (0x108AB, 0x108AC, 4, 1),
    (0x108AD, 0x108AE, 10, 10),
    (0x108AF, 0x108AF, 100, 0),
    (0x108FB, 0x108FC, 1, 4),
    (0x108FD, 0x108FE, 10, 10),
    (0x108FF, 0x108FF, 100, 0),
    (0x10916, 0x10917, 1, 9),
    (0x10918, 0x10919, 20, 80),
    (0x1091A, 0x1091B, 2, 1),
    (0x109BC, 0x109BD, -2, 0),
    (0x109C0, 0x109C9, 1, 1),
    (0x109CA, 0x109CF, 20, 10),
    (0x109D2, 0x109DB, 100, 100),
    (0x109DC, 0x109E4, 2000, 1000),
    (0x109E5, 0x109ED, 20000, 10000),
    (0x109EE, 0x109F5, 200000, 100000),
    (0x109F6, 0x109FF, -2, 0),
    (0x10A40, 0x10A43, 1, 1),
    (0x10A44, 0x10A45, 10, 10),
    (0x10A46, 0x10A47, 100, 900),
    (0x10A48, 0x10A48, -2, 0),
    (0x10A7D, 0x10A7E, 1, 49),
    (0x10A9D, 0x10A9E, 1, 9),
    (0x10A9F, 0x10A9F, 20, 0),
    (0x10AEB, 0x10AEC, 1, 4),
    (0x10AED, 0x10AEE, 10, 10),
    (0x10AEF, 0x10AEF, 100, 0),
    (0x10B58, 0x10B5B, 1, 1),
    (0x10B5C, 0x10B5D, 10, 10),
    (0x10B5E, 0x10B5F, 100, 900),
    (0x10B78, 0x10B7B, 1, 1),
    (0x10B7C, 0x10B7D, 10, 10),
    (0x10B7E, 0x10B7F, 100, 900),
    (0x10BA9, 0x10BAC, 1, 1),
    (0x10BAD, 0x10BAE, 10, 10),
    (0x10BAF, 0x10BAF, 100, 0),
    (0x10CFA, 0x10CFB, 1, 4),
    (0x10CFC, 0x10CFD, 10, 40),
    (0x10CFE, 0x10CFF, 100, 900),
    (0x10E60, 0x10E69, 1, 1),
    (0x10E6A, 0x10E72, 20, 10),
    (0x10E73, 0x10E7A, 200, 100),
    (0x10E7B, 0x10E7E, -2, 0),
    (0x10F1D, 0x10F21, 1, 1),
    (0x10F22, 0x10F24, 10, 10),
    (0x10F25, 0x10F26, 100, -102),
    (0x10F51, 0x10F52, 1, 9),
    (0x10F53, 0x10F54, 20, 80),
    (0x10FC5, 0x10FC8, 1, 1),
    (0x10FC9, 0x10FCA, 10, 10),
    (0x10FCB, 0x10FCB, 100, 0),
    (0x11052, 0x1105B, 1, 1),
    (0x1105C, 0x11064, 20, 10),
    (0x11065, 0x11065, 1000, 0),
    (0x111E1, 0x111EA, 1, 1),
    (0x111EB, 0x111F3, 20, 10),
    (0x111F4, 0x111F4, 1000, 0),
    (0x1173A, 0x1173B, 10, 10),
    (0x118EA, 0x118F2, 10, 10),
    (0x11C5A, 0x11C63, 1, 1),
    (0x11C64, 0x11C6C, 20, 10),
    (0x11FC0, 0x11FD4, -2, 0),
    (0x12400, 0x12407, 2, 1),
    (0x12408, 0x1240E, 3, 1),
    (0x1240F, 0x12414, 4, 1),
    (0x12415, 0x1241D, 1, 1),
    (0x1241E, 0x12422, 1, 1),
    (0x12423, 0x12424, 2, 1),
    (0x12425, 0x1242B, 3, 1),
    (0x1242C, 0x1242E, 1, 1),
    (0x1242F, 0x12431, 3, 1),
    (0x12432, 0x12433, 216000, 216000),
    (0x12434, 0x12436, 1, 1),
    (0x12437, 0x12439, 3, 1),
    (0x1243A, 0x1243B, 3, 0),
    (0x1243C, 0x1243F, 4, 0),
    (0x12440, 0x12441, 6, 1),
    (0x12442, 0x12443, 7, 0),
    (0x12444, 0x12445, 8, 0),
    (0x12446, 0x12449, 9, 0),
    (0x1244A, 0x1244E, 2, 1),
    (0x1244F, 0x12452, 1, 1),
    (0x12453, 0x12454, 4, 1),
    (0x12455, 0x12456, 5, -3),
    (0x12457, 0x12458, 3, -2),
    (0x12459, 0x1245A, 2, -4),
    (0x1245B, 0x12466, -2, 0),
    (0x12467, 0x12468, 40, 10),
    (0x12469, 0x1246E, 4, 1),
    (0x16B5B, 0x16B5C, 10, 90),
    (0x16B5D, 0x16B5E, 10000, 990000),
    (0x16B5F, 0x16B60, 100000000, -100000002),
    (0x16B61, 0x16B61, -2, 0),
    (0x16E80, 0x16E93, 0, 1),
    (0x16E94, 0x16E96, 1, 1),
    (0x1D2C0, 0x1D2D3, 0, 1),
    (0x1D2E0, 0x1D2F3, 0, 1),
    (0x1D360, 0x1D369, 1, 1),
    (0x1D36A, 0x1D371, 20, 10),
    (0x1D372, 0x1D376, 1, 1),
    (0x1D377, 0x1D378, 1, 4),
    (0x1E8C7, 0x1E8CF, 1, 1),
    (0x1EC71, 0x1EC7A, 1, 1),
    (0x1EC7B, 0x1EC83, 20, 10),
    (0x1EC84, 0x1EC8C, 200, 100),
    (0x1EC8D, 0x1EC95, 2000, 1000),
    (0x1EC96, 0x1EC9E, 20000, 10000),
    (0x1EC9F, 0x1ECA0, 200000, -100000),
    (0x1ECA1, 0x1ECA2, 10000000, 10000000),
    (0x1ECA3, 0x1ECAB, 1, 1),
    (0x1ECAD, 0x1ECAF, -2, 0),
    (0x1ECB1, 0x1ECB2, 1, 1),
    (0x1ECB3, 0x1ECB4, 10000, 90000),
    (0x1ED01, 0x1ED0A, 1, 1),
    (0x1ED0B, 0x1ED13, 20, 10),
    (0x1ED14, 0x1ED1C, 200, 100),
    (0x1ED1D, 0x1ED25, 2000, 1000),
    (0x1ED26, 0x1ED2D, 20000, 10000),
    (0x1ED2F, 0x1ED37, 2, 1),
    (0x1ED38, 0x1ED39, 400, 200),
    (0x1ED3A, 0x1ED3B, 2000, 8000),
    (0x1ED3C, 0x1ED3D, -2, 0),
    (0x1F100, 0x1F101, 0, 0),
    (0x1F102, 0x1F10A, 1, 1),
    (0x1F10B, 0x1F10C, 0, 0),
    (0x2F890, 0x2F890, 9, 0),
];

/// Unicode `Nd` (decimal digit) ranges, inclusive.
/// `Character.isDigit`'s `Nd` ranges (inclusive), generated from the JDK's own
/// answers so the runtime agrees with the JVM's Unicode version.
const DIGIT: &[(u32, u32)] = &[
    (0x30, 0x39),
    (0x660, 0x669),
    (0x6F0, 0x6F9),
    (0x7C0, 0x7C9),
    (0x966, 0x96F),
    (0x9E6, 0x9EF),
    (0xA66, 0xA6F),
    (0xAE6, 0xAEF),
    (0xB66, 0xB6F),
    (0xBE6, 0xBEF),
    (0xC66, 0xC6F),
    (0xCE6, 0xCEF),
    (0xD66, 0xD6F),
    (0xDE6, 0xDEF),
    (0xE50, 0xE59),
    (0xED0, 0xED9),
    (0xF20, 0xF29),
    (0x1040, 0x1049),
    (0x1090, 0x1099),
    (0x17E0, 0x17E9),
    (0x1810, 0x1819),
    (0x1946, 0x194F),
    (0x19D0, 0x19D9),
    (0x1A80, 0x1A89),
    (0x1A90, 0x1A99),
    (0x1B50, 0x1B59),
    (0x1BB0, 0x1BB9),
    (0x1C40, 0x1C49),
    (0x1C50, 0x1C59),
    (0xA620, 0xA629),
    (0xA8D0, 0xA8D9),
    (0xA900, 0xA909),
    (0xA9D0, 0xA9D9),
    (0xA9F0, 0xA9F9),
    (0xAA50, 0xAA59),
    (0xABF0, 0xABF9),
    (0xFF10, 0xFF19),
    (0x104A0, 0x104A9),
    (0x10D30, 0x10D39),
    (0x10D40, 0x10D49),
    (0x11066, 0x1106F),
    (0x110F0, 0x110F9),
    (0x11136, 0x1113F),
    (0x111D0, 0x111D9),
    (0x112F0, 0x112F9),
    (0x11450, 0x11459),
    (0x114D0, 0x114D9),
    (0x11650, 0x11659),
    (0x116C0, 0x116C9),
    (0x116D0, 0x116E3),
    (0x11730, 0x11739),
    (0x118E0, 0x118E9),
    (0x11950, 0x11959),
    (0x11BF0, 0x11BF9),
    (0x11C50, 0x11C59),
    (0x11D50, 0x11D59),
    (0x11DA0, 0x11DA9),
    (0x11F50, 0x11F59),
    (0x16130, 0x16139),
    (0x16A60, 0x16A69),
    (0x16AC0, 0x16AC9),
    (0x16B50, 0x16B59),
    (0x16D70, 0x16D79),
    (0x1CCF0, 0x1CCF9),
    (0x1D7CE, 0x1D7FF),
    (0x1E140, 0x1E149),
    (0x1E2F0, 0x1E2F9),
    (0x1E4F0, 0x1E4F9),
    (0x1E5F1, 0x1E5FA),
    (0x1E950, 0x1E959),
    (0x1FBF0, 0x1FBF9),
];

/// Unicode `Zs`/`Zl`/`Zp` (space separator) ranges, inclusive.
/// `Character.isWhitespace` ranges (inclusive), generated from the JDK.
const WHITESPACE: &[(u32, u32)] = &[
    (0x9, 0xD),
    (0x1C, 0x20),
    (0x1680, 0x1680),
    (0x2000, 0x2006),
    (0x2008, 0x200A),
    (0x2028, 0x2029),
    (0x205F, 0x205F),
    (0x3000, 0x3000),
];

/// `Character.isSpaceChar`'s `Zs`/`Zl`/`Zp` ranges (inclusive), generated from
/// the JDK.
const SPACE: &[(u32, u32)] = &[
    (0x20, 0x20),
    (0xA0, 0xA0),
    (0x1680, 0x1680),
    (0x2000, 0x200A),
    (0x2028, 0x2029),
    (0x202F, 0x202F),
    (0x205F, 0x205F),
    (0x3000, 0x3000),
];

/// The exception object a builtin raises for an input Java rejects: an instance
/// of `class` whose `detailMessage` is `message`, shaped the way
/// `Interpreter::new_exception` shapes one (the class's own `(String)`
/// constructor cannot run from a heap-only builtin, so the field the JDK's
/// `getMessage` reads is set directly).
pub(crate) fn exception(heap: &mut Heap, class: &str, message: &str) -> Value {
    let text = Value::Object(Some(heap.alloc_string(message)));
    let mut fields = std::collections::HashMap::new();
    fields.insert("detailMessage".to_string(), text);
    Value::Object(Some(heap.alloc(crate::value::JavaObject::Instance {
        class_name: class.to_string(),
        fields,
    })))
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
            | "java/lang/Integer"
            | "java/lang/Short"
            | "java/lang/Byte"
            | "java/lang/Character"
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
            // A boxed value must render as its own class:
            // `Integer.valueOf(5).toString()` is "5" and
            // `Double.valueOf(1.5).toString()` is "1.5", not the `Object`
            // defaults a heap object with only a `value` field would give.
            ("toString", Some(b')')) if descriptor == "()Ljava/lang/String;" => {
                let text = boxed_text(class_name, &unboxed(args.first()?));
                let sid = heap.alloc_string_uninterned(text);
                return handled(Value::Object(Some(sid)));
            }
            ("equals", Some(b'L')) => {
                // A boxed number is the plain value here, so `Integer.valueOf(5)`
                // and `5` are the same value and the argument has no class to
                // check; Java's own rule (equal only to the same wrapper class)
                // cannot be applied to a value that does not carry its box.
                let left = unboxed(args.first()?);
                let right = unboxed(args.get(1)?);
                return handled(Value::from_bool(left == right));
            }
            ("hashCode", Some(b')')) => {
                let value = unboxed(args.first()?);
                let hash = match class_name {
                    "java/lang/Double" => {
                        let bits = value.as_double().to_bits();
                        (bits ^ (bits >> 32)) as i32
                    }
                    "java/lang/Float" => value.as_float().to_bits() as i32,
                    "java/lang/Long" => {
                        let v = value.as_long();
                        (v ^ (v >> 32)) as i32
                    }
                    "java/lang/Short" | "java/lang/Byte" => value.as_int() as i16 as i32,
                    _ => value.as_int(),
                };
                return handled(Value::Int(hash));
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
            // `parseInt(String)`, `parseInt(String, int)`, and `valueOf` of the
            // same shapes: Java's grammar (an optional sign then digits in the
            // radix, no surrounding space) and `NumberFormatException` (never a
            // silent 0) when the text is not a number or overflows.
            "parseInt" | "valueOf"
                if args.first().is_some_and(|v| string_of(heap, v).is_some()) =>
            {
                let text = string_of(heap, &args[0])?;
                let radix = if args.len() > 1 { int(1)? } else { 10 };
                match parse_integral(&text, radix)
                    .and_then(|value| i32::try_from(value).map_err(|_| ()))
                {
                    Ok(value) => handled(Value::Int(value)),
                    Err(()) => threw(
                        heap,
                        "java/lang/NumberFormatException",
                        &format!("For input string: \"{text}\""),
                    ),
                }
            }
            "parseUnsignedInt" => {
                let text = string_of(heap, &args[0])?;
                let radix = if args.len() > 1 { int(1)? } else { 10 };
                match parse_unsigned(&text, radix) {
                    Some(value) if value <= u32::MAX as u64 => {
                        handled(Value::Int(value as u32 as i32))
                    }
                    _ => threw(
                        heap,
                        "java/lang/NumberFormatException",
                        &format!("For input string: \"{text}\""),
                    ),
                }
            }
            "toString"
                if descriptor.starts_with("(I)")
                    || descriptor.starts_with("(II)")
                    || descriptor == "()Ljava/lang/String;" =>
            {
                // `Integer.toString()` is the plain int; the two-argument form
                // writes it in `radix` (with a minus sign, not two's complement).
                let radix = if args.len() > 1 { int(1)? } else { 10 };
                let text = radix_string(int(0)? as i64, radix);
                let sid = heap.alloc_string(text);
                handled(Value::Object(Some(sid)))
            }
            "toHexString" => {
                let sid = heap.alloc_string(unsigned_string(int(0)? as u32 as u64, 16));
                handled(Value::Object(Some(sid)))
            }
            "toBinaryString" => {
                let sid = heap.alloc_string(unsigned_string(int(0)? as u32 as u64, 2));
                handled(Value::Object(Some(sid)))
            }
            "toOctalString" => {
                let sid = heap.alloc_string(unsigned_string(int(0)? as u32 as u64, 8));
                handled(Value::Object(Some(sid)))
            }
            "toUnsignedString" => {
                let radix = if args.len() > 1 { int(1)? } else { 10 };
                let sid = heap.alloc_string(unsigned_string(int(0)? as u32 as u64, radix));
                handled(Value::Object(Some(sid)))
            }
            "toUnsignedLong" => handled(Value::Long(int(0)? as u32 as i64)),
            "divideUnsigned" => handled(Value::Int(
                ((int(0)? as u32 as u64) / (int(1)? as u32 as u64)) as u32 as i32,
            )),
            "remainderUnsigned" => handled(Value::Int(
                ((int(0)? as u32 as u64) % (int(1)? as u32 as u64)) as u32 as i32,
            )),
            "compareUnsigned" => {
                handled(Value::Int(match (int(0)? as u32).cmp(&(int(1)? as u32)) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                }))
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
                let radix = if args.len() > 1 { int(1)? } else { 10 };
                match parse_integral(&text, radix) {
                    Ok(value) => handled(Value::Long(value)),
                    Err(()) => threw(
                        heap,
                        "java/lang/NumberFormatException",
                        &format!("For input string: \"{text}\""),
                    ),
                }
            }
            "parseUnsignedLong" => {
                let text = string_of(heap, &args[0])?;
                let radix = if args.len() > 1 { int(1)? } else { 10 };
                match parse_unsigned(&text, radix) {
                    Some(value) => handled(Value::Long(value as i64)),
                    None => threw(
                        heap,
                        "java/lang/NumberFormatException",
                        &format!("For input string: \"{text}\""),
                    ),
                }
            }
            "toString"
                if descriptor.starts_with("(J)")
                    || descriptor.starts_with("(JI)")
                    || descriptor == "()Ljava/lang/String;" =>
            {
                let radix = if args.len() > 1 { int(1)? } else { 10 };
                let sid = heap.alloc_string(radix_string(long(0)?, radix));
                handled(Value::Object(Some(sid)))
            }
            "toHexString" => {
                let sid = heap.alloc_string(unsigned_string(long(0)? as u64, 16));
                handled(Value::Object(Some(sid)))
            }
            "toBinaryString" => {
                let sid = heap.alloc_string(unsigned_string(long(0)? as u64, 2));
                handled(Value::Object(Some(sid)))
            }
            "toOctalString" => {
                let sid = heap.alloc_string(unsigned_string(long(0)? as u64, 8));
                handled(Value::Object(Some(sid)))
            }
            "toUnsignedString" => {
                let radix = if args.len() > 1 { int(1)? } else { 10 };
                let sid = heap.alloc_string(unsigned_string(long(0)? as u64, radix));
                handled(Value::Object(Some(sid)))
            }
            "divideUnsigned" => {
                handled(Value::Long(((long(0)? as u64) / (long(1)? as u64)) as i64))
            }
            "remainderUnsigned" => {
                handled(Value::Long(((long(0)? as u64) % (long(1)? as u64)) as i64))
            }
            "compareUnsigned" => handled(Value::Int(
                match (long(0)? as u64).cmp(&(long(1)? as u64)) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                },
            )),
            _ => None,
        },
        "java/lang/Float" => match method_name {
            // `Float.parseFloat` is `Double.parseDouble` narrowed, so it accepts
            // exactly the same grammar (including a hex float, and `1e400`
            // becoming an infinity) and rejects the same strings.
            "parseFloat" | "valueOf"
                if args.first().is_some_and(|v| string_of(heap, v).is_some()) =>
            {
                let text = string_of(heap, &args[0])?;
                match parse_floating(&text) {
                    Some(value) => handled(Value::Float(value as f32)),
                    None => threw(
                        heap,
                        "java/lang/NumberFormatException",
                        &format!("For input string: \"{text}\""),
                    ),
                }
            }
            "compare" => handled(Value::Int(compare_float(
                args.first()?.as_float(),
                args.get(1)?.as_float(),
            ))),
            "isNaN" if descriptor.starts_with("(F)") => {
                handled(Value::from_bool(args.first()?.as_float().is_nan()))
            }
            "isInfinite" => handled(Value::from_bool(args.first()?.as_float().is_infinite())),
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
            "max" | "min" => {
                let (a, b) = (args.first()?.as_float(), args.get(1)?.as_float());
                handled(Value::Float(if method_name == "max" {
                    max_float(a, b)
                } else {
                    min_float(a, b)
                }))
            }
            "sum" => handled(Value::Float(
                args.first()?.as_float() + args.get(1)?.as_float(),
            )),
            "isFinite" => handled(Value::from_bool(args.first()?.as_float().is_finite())),
            "toHexString" => {
                let sid = heap.alloc_string(java_float_hex(args.first()?.as_float()));
                handled(Value::Object(Some(sid)))
            }
            _ => None,
        },
        "java/lang/Double" => match method_name {
            "parseDouble" | "valueOf"
                if args.first().is_some_and(|v| string_of(heap, v).is_some()) =>
            {
                let text = string_of(heap, &args[0])?;
                match parse_floating(&text) {
                    Some(value) => handled(Value::Double(value)),
                    None => threw(
                        heap,
                        "java/lang/NumberFormatException",
                        &format!("For input string: \"{text}\""),
                    ),
                }
            }
            "compare" => handled(Value::Int(compare_double(
                args.first()?.as_double(),
                args.get(1)?.as_double(),
            ))),
            "isNaN" if descriptor.starts_with("(D)") => {
                handled(Value::from_bool(args.first()?.as_double().is_nan()))
            }
            "isInfinite" => handled(Value::from_bool(args.first()?.as_double().is_infinite())),
            // `doubleToRawLongBits` reports the value's own bits (a NaN keeps its
            // payload); `doubleToLongBits` canonicalizes every NaN to one.
            "doubleToRawLongBits" => {
                handled(Value::Long(args.first()?.as_double().to_bits() as i64))
            }
            "doubleToLongBits" => {
                let bits = args.first()?.as_double().to_bits();
                handled(Value::Long(if f64::from_bits(bits).is_nan() {
                    0x7ff8_0000_0000_0000
                } else {
                    bits as i64
                }))
            }
            "longBitsToDouble" => handled(Value::Double(f64::from_bits(long(0)? as u64))),
            "max" | "min" => {
                let (a, b) = (args.first()?.as_double(), args.get(1)?.as_double());
                handled(Value::Double(if method_name == "max" {
                    max_double(a, b)
                } else {
                    min_double(a, b)
                }))
            }
            "sum" => handled(Value::Double(
                args.first()?.as_double() + args.get(1)?.as_double(),
            )),
            "isFinite" => handled(Value::from_bool(args.first()?.as_double().is_finite())),
            "toHexString" => {
                let sid = heap.alloc_string(java_double_hex(args.first()?.as_double()));
                handled(Value::Object(Some(sid)))
            }
            _ => None,
        },
        "java/lang/Character" => {
            // A `char` argument is ONE UTF-16 code unit, so a value above
            // 0xFFFF is a lone surrogate, not an astral character (Java's
            // `char` cannot hold an astral character at all).
            let unit = int(0)?;
            let c = char::from_u32(unit as u32);
            match method_name {
                // A boxed `Character` is the plain char value, as `Integer` is
                // the plain int, so boxing and unboxing return the char itself.
                "valueOf" | "charValue" => handled(Value::Int(unit)),
                "isDigit" => handled(Value::from_bool(c.is_some_and(is_decimal_digit))),
                "isLetter" => handled(Value::from_bool(c.is_some_and(is_letter))),
                "isLetterOrDigit" => handled(Value::from_bool(
                    c.is_some_and(|c| is_letter(c) || is_decimal_digit(c)),
                )),
                // Not the same predicate: ` ` (NBSP) is a space character
                // but not whitespace, and `	` is whitespace but not a space
                // character.
                "isWhitespace" => handled(Value::from_bool(c.is_some_and(is_whitespace))),
                "isSpaceChar" => handled(Value::from_bool(c.is_some_and(is_space_char))),
                "isUpperCase" => handled(Value::from_bool(c.is_some_and(|c| c.is_uppercase()))),
                "isLowerCase" => handled(Value::from_bool(c.is_some_and(|c| c.is_lowercase()))),
                "isTitleCase" => handled(Value::from_bool(c.is_some_and(is_title_case))),
                "isAlphabetic" => handled(Value::from_bool(c.is_some_and(|c| c.is_alphabetic()))),
                "isIdeographic" => handled(Value::from_bool(c.is_some_and(is_ideographic))),
                "isJavaIdentifierStart" | "isJavaIdentifierPart" => {
                    let part = c.is_some_and(is_java_identifier_part);
                    handled(Value::from_bool(
                        if method_name == "isJavaIdentifierStart" {
                            part && !c.is_some_and(|c| c.is_ascii_digit() || is_decimal_digit(c))
                        } else {
                            part
                        },
                    ))
                }
                "toLowerCase" => handled(Value::Int(
                    c.and_then(|c| c.to_lowercase().next())
                        .map_or(unit, |c| c as i32),
                )),
                "toUpperCase" => handled(Value::Int(
                    c.and_then(|c| c.to_uppercase().next())
                        .map_or(unit, |c| c as i32),
                )),
                "getNumericValue" => handled(Value::Int(c.map_or(-1, numeric_value))),
                "digit" => handled(Value::Int(digit(unit, int(1)? as u32))),
                "forDigit" => {
                    // A digit outside the radix answers NUL, which is what
                    // `Character.forDigit` returns.
                    let (value, radix) = (int(0)?, int(1)?);
                    let out = if (0..radix).contains(&value) && (2..=36).contains(&radix) {
                        char::from_digit(value as u32, radix as u32).unwrap_or('\0')
                    } else {
                        '\0'
                    };
                    handled(Value::Int(out as i32))
                }
                "charCount" => handled(Value::Int(if int(0)? >= 0x10000 { 2 } else { 1 })),
                // `Character.toChars(int)` answers a `char[]`: one unit for a
                // BMP value (a surrogate is a legal BMP value and comes back
                // unchanged), the surrogate PAIR for a supplementary code point.
                // Returning the code point as a bare `int` made the caller treat
                // it as a `char[]` and append nothing, so
                // `StringBuilder.append(Character.toChars(c))` silently dropped
                // its text. Only a value outside 0..=0x10FFFF is not a code
                // point at all, and the JDK throws for exactly those.
                "toChars" => {
                    let code_point = int(0)?;
                    let elements: Vec<Value> = if (0..=0xFFFF).contains(&code_point) {
                        // `isBmpCodePoint`: the value IS one code unit.
                        vec![Value::Int(code_point)]
                    } else if (0..=0x10FFFF).contains(&code_point) {
                        let value = code_point as u32;
                        let high = 0xD800 + ((value - 0x10000) >> 10) as i32;
                        let low = 0xDC00 + ((value - 0x10000) & 0x3FF) as i32;
                        vec![Value::Int(high), Value::Int(low)]
                    } else {
                        return threw(
                            heap,
                            "java/lang/IllegalArgumentException",
                            &format!("Not a valid Unicode code point: 0x{code_point:X}"),
                        );
                    };
                    let id = heap.alloc(crate::value::JavaObject::Array {
                        elem_type: "char".into(),
                        elements,
                    });
                    handled(Value::Object(Some(id)))
                }
                // `Character.toCodePoint(char, char)` is the inverse: the
                // supplementary code point a valid high/low surrogate pair
                // encodes, or the JDK's `(high << 10) + low + (0x10000 - (0xD800
                // << 10) - 0xDC00)` for a pair that is not one.
                "toCodePoint" => {
                    let (high, low) = (int(0)?, int(1)?);
                    handled(Value::Int(
                        (high << 10) + low + (0x10000 - (0xD800 << 10) - 0xDC00),
                    ))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// `Integer.toString(int, radix)` / `Long.toString(long, radix)`: the value with
/// a leading minus sign, in `radix`. A radix outside 2..=36 falls back to 10
/// (the JDK's own behaviour, which uses 10 when `radix < MIN_RADIX`).
fn radix_string(value: i64, radix: i32) -> String {
    let radix = if (2..=36).contains(&radix) {
        radix as u32
    } else {
        10
    };
    if radix == 10 {
        return value.to_string();
    }
    if value == 0 {
        return "0".into();
    }
    // Take the magnitude in `i64` without overflowing on `LONG_MIN`: its
    // magnitude does not fit, so accumulate the negative form instead and
    // negate the digits, as the JDK's `Long.toString` does.
    let negative = value < 0;
    let mut value = if negative { value } else { -value };
    let mut digits = Vec::new();
    while value != 0 {
        let digit = -(value % radix as i64) as u32;
        digits.push(char::from_digit(digit, radix).unwrap_or('0'));
        value /= radix as i64;
    }
    if negative {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

/// The unsigned form Java uses for `toHexString`, `toBinaryString`,
/// `toOctalString` and the unsigned `toString` overloads.
fn unsigned_string(value: u64, radix: i32) -> String {
    if !(2..=36).contains(&radix) {
        return unsigned_string(value, 10);
    }
    if value == 0 {
        return "0".into();
    }
    let mut value = value;
    let mut digits = Vec::new();
    while value != 0 {
        let digit = (value % radix as u64) as u32;
        digits.push(char::from_digit(digit, radix as u32).unwrap_or('0'));
        value /= radix as u64;
    }
    digits.iter().rev().collect()
}

/// The exception a builtin raises, as `BuiltinResult::Threw`.
fn threw(heap: &mut Heap, class: &str, message: &str) -> Option<BuiltinResult> {
    Some(BuiltinResult::Threw(exception(heap, class, message)))
}

/// Java's integral parser: `[+|-]digits` in `radix`, no surrounding space, at
/// least one digit, and no overflow of the `long` range. `Integer.parseInt`
/// narrows the answer to `int`; `Long.parseLong` is this directly.
///
/// A null string throws `NumberFormatException` too (the JDK's own message is
/// "Cannot parse null string"), and a radix outside 2..=36 throws rather than
/// defaulting, which is what the real methods do.
fn parse_integral(text: &str, radix: i32) -> Result<i64, ()> {
    if !(2..=36).contains(&radix) {
        return Err(());
    }
    let radix = radix as u32;
    let mut units = text.chars();
    let Some(first) = units.next() else {
        return Err(());
    };
    let negative = match first {
        '+' => false,
        '-' => true,
        _ => {
            // Not a sign: put it back and parse from the start.
            units = text.chars();
            false
        }
    };
    let mut value: i64 = 0;
    let mut digits = 0usize;
    for c in units {
        // `Character.digit` over the whole `char`: Java accepts the Unicode
        // decimal digits, so `Integer.parseInt("٣")` is 3.
        let digit = digit(c as i32, radix);
        if digit < 0 {
            return Err(());
        }
        // Accumulate negatively so `LONG_MIN` fits, as `parseLong` does.
        value = value.checked_mul(radix as i64).ok_or(())?;
        value = value.checked_sub(digit as i64).ok_or(())?;
        digits += 1;
    }
    if digits == 0 {
        return Err(());
    }
    if negative {
        Ok(value)
    } else {
        value.checked_neg().ok_or(())
    }
}

/// The unsigned parser behind `Integer.parseUnsignedInt` and
/// `Long.parseUnsignedLong`: the same grammar, but the value may fill the
/// unsigned range and a leading minus sign is rejected.
fn parse_unsigned(text: &str, radix: i32) -> Option<u64> {
    if !(2..=36).contains(&radix) || text.starts_with('-') {
        return None;
    }
    let radix = radix as u32;
    let digits = text.strip_prefix('+').unwrap_or(text);
    if digits.is_empty() {
        return None;
    }
    let mut value: u64 = 0;
    for c in digits.chars() {
        let digit = digit(c as i32, radix);
        if digit < 0 {
            return None;
        }
        value = value.checked_mul(radix as u64)?;
        value = value.checked_add(digit as u64)?;
    }
    Some(value)
}

/// Java's `Double.parseDouble` / `Float.parseFloat` grammar, which the Rust
/// parser does not match:
///
/// - surrounding whitespace is allowed and trimmed (`" 1.0 "` is 1.0);
/// - an explicit type suffix `f`/`F`/`d`/`D` is allowed (`"1.0f"` is 1.0);
/// - a hexadecimal float is allowed (`0x1.8p1`, `0x1p-2`);
/// - `NaN` and `Infinity` are allowed with an optional sign, but only spelled
///   exactly like that (`inf`, `NAN` and `Infinityf` are rejected);
/// - `inf`/`infinity` in any other case, `1_0`, `1e` and `0x1` are rejected.
///
/// Overflow to infinity is not an error (`1e400` is `Infinity`), which Rust's
/// parser already does.
fn parse_floating(text: &str) -> Option<f64> {
    let text = text.trim();
    let (negative, body) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    // A sign with nothing after it only matches the bare sign strings, which
    // are not numbers: Java rejects "+" and "-". A second sign is not part of
    // the grammar either (`+-1`, `--1`), even though Rust's parser would take
    // it.
    if body.is_empty() || body.starts_with(['+', '-']) {
        return None;
    }
    let value = match body {
        // The spellings are exact: `NaN` and `Infinity`, with the initial
        // capital only. Rust's parser accepts `nan`, `inf`, `INFINITY` and the
        // rest case-insensitively, which Java rejects.
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        _ => parse_decimal_or_hex(body)?,
    };
    // Negating a NaN would set its sign bit; Java's parsed NaN is the canonical
    // positive one whatever sign the text carried.
    if negative && !value.is_nan() {
        Some(-value)
    } else {
        Some(value)
    }
}

/// The numeric body of a Java floating literal, without sign or `NaN`/
/// `Infinity`, with an optional `f`/`F`/`d`/`D` suffix.
fn parse_decimal_or_hex(body: &str) -> Option<f64> {
    let mantissa = match body.as_bytes().last() {
        Some(b'f' | b'F' | b'd' | b'D') => &body[..body.len() - 1],
        _ => body,
    };
    if mantissa.is_empty() {
        return None;
    }
    let hex = mantissa.len() > 2 && (mantissa.starts_with("0x") || mantissa.starts_with("0X"));
    if hex {
        return parse_hex_float(&mantissa[2..]);
    }
    // A decimal body: Rust's parser accepts exactly the same spellings Java
    // does here (`5.`, `.5`, `1e5`, `1E+5`) and rejects `1e`, `1.2.3`, `1_0`
    // and `inf`/`NaN`-like words that are not the exact capitalisation above.
    // Its one extra liberty is the case-insensitive `inf`/`nan`, which the
    // template above already consumed, so a body containing a letter other
    // than an exponent `e`/`E` is rejected here.
    if mantissa
        .chars()
        .any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
    {
        return None;
    }
    mantissa.parse::<f64>().ok()
}

/// A Java hexadecimal float body (without the `0x`), as in `0x1.8p1` or
/// `0x1p-2`: a hexadecimal mantissa with an optional point, and a mandatory
/// binary exponent after `p`/`P`. Rust's parser does not accept this form at
/// all, so it is decoded here.
fn parse_hex_float(body: &str) -> Option<f64> {
    let (mantissa, exponent) = body.split_once(['p', 'P'])?;
    let exponent: i32 = match exponent.as_bytes().first() {
        Some(b'+') => exponent[1..].parse().ok()?,
        Some(b'-') => -exponent[1..].parse::<i32>().ok()?,
        _ => exponent.parse().ok()?,
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (mantissa, ""),
    };
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let mut value = 0.0f64;
    let mut digits = 0usize;
    for c in whole.chars().chain(fraction.chars()) {
        let digit = c.to_digit(16)?;
        value = value * 16.0 + digit as f64;
        digits += 1;
    }
    if digits == 0 {
        return None;
    }
    // The point sits after `whole`, so shift the binary exponent by the
    // fraction's nibble count: each hex digit is four bits.
    let shift = exponent - 4 * fraction.len() as i32;
    Some(value * 2f64.powi(shift))
}

/// Java's `Double.toHexString`: `0x1.<hex>p<exp>` with at least one fraction
/// digit (a trailing zero is kept), `0x0.0p0` for zero, and `NaN`/`Infinity`.
fn java_double_hex(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let bits = value.abs().to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    if exponent == 0 && fraction == 0 {
        return format!("{sign}0x0.0p0");
    }
    let (lead, fraction, exp) = if exponent == 0 {
        // A subnormal: the JDK writes it as 0x0.<fraction>p-1022.
        (0, fraction, -1022)
    } else {
        (1, fraction, exponent - 1023)
    };
    let hex = format!("{fraction:013x}");
    // The JDK keeps one digit after the point and strips the rest.
    let trimmed = hex.trim_end_matches('0');
    let digits = if trimmed.is_empty() { "0" } else { trimmed };
    format!("{sign}0x{lead}.{digits}p{exp}")
}

/// Java's `Float.toHexString`, the `f32` form of [`java_double_hex`].
fn java_float_hex(value: f32) -> String {
    java_double_hex(value as f64).replace("p", "p")
}

/// Java's `Character.isTitleCase`: the category is `Lt`.
fn is_title_case(c: char) -> bool {
    matches!(c as u32,
        0x01C5 | 0x01C8 | 0x01CB | 0x01F2 | 0x1F88..=0x1F8F | 0x1F98..=0x1F9F
            | 0x1FA8..=0x1FAF | 0x1FBC | 0x1FCC | 0x1FFC)
}

/// Java's `Character.isIdeographic`: the character is in one of the ideograph
/// blocks (CJK unified ideographs, compatibility ideographs, and their
/// extensions).
fn is_ideographic(c: char) -> bool {
    matches!(c as u32,
        0x3006 | 0x3007 | 0x3021..=0x3029 | 0x3038..=0x303A | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF | 0xF900..=0xFA2D | 0x20000..=0x2FA1F)
}

/// `Character.isJavaIdentifierPart`, as far as the standard library's tables
/// answer it: a letter, a digit, a combining mark, a currency symbol or a
/// connector punctuation character.
fn is_java_identifier_part(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphanumeric() || is_combining_mark(c)
}

/// The Unicode `M*` categories, which the standard library exposes only as
/// "not alphabetic, not numeric, not whitespace but assigned": the combining
/// diacritical marks are the ones `is_alphabetic` reports as `Other_Alphabetic`
/// and the rest are the `Mn`/`Mc`/`Me` blocks.
fn is_combining_mark(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A
            | 0x064B..=0x065F | 0x0900..=0x0903 | 0x093A..=0x094F | 0x0E31
            | 0x0E34..=0x0E3A | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20F0
            | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F)
}
