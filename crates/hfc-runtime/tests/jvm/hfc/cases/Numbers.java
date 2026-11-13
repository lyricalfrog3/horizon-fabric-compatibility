package hfc.cases;

/**
 * Numeric semantics of `Double`, `Float`, `Math`, `Integer` and `Long`:
 * comparison ordering (NaN and signed zero), raw versus canonical NaN bits,
 * `Math.min`/`Math.max`/`Math.abs`, the parsers' grammar and their
 * `NumberFormatException` behaviour, and the radix/unsigned string helpers.
 * Every line is deterministic and printed so that float formatting the
 * runtime is known to differ on (Java's `Double.toString`) is not involved.
 */
public final class Numbers {
    static String hex(long v) {
        return Long.toHexString(v);
    }

    static String hexDouble(double v) {
        return hex(Double.doubleToRawLongBits(v));
    }

    static String hexFloat(float v) {
        return Integer.toHexString(Float.floatToRawIntBits(v));
    }

    static String nan_compare() {
        return Double.compare(Double.NaN, Double.NaN) + ","
                + Double.compare(Double.NaN, 1.0) + ","
                + Double.compare(1.0, Double.NaN) + ","
                + Double.compare(Double.NaN, Double.POSITIVE_INFINITY) + ","
                + Double.compare(Double.NEGATIVE_INFINITY, Double.NaN) + ","
                + Double.compare(Double.NaN, Double.NEGATIVE_INFINITY) + ","
                + Float.compare(Float.NaN, Float.NaN) + ","
                + Float.compare(Float.NaN, 1.0f) + ","
                + Float.compare(1.0f, Float.NaN);
    }

    static String zero_compare() {
        return Double.compare(-0.0, 0.0) + ","
                + Double.compare(0.0, -0.0) + ","
                + Double.compare(0.0, 0.0) + ","
                + Double.compare(-0.0, -0.0) + ","
                + Float.compare(-0.0f, 0.0f) + ","
                + Float.compare(0.0f, -0.0f) + ","
                + Float.compare(0.0f, 0.0f) + ","
                + Double.compare(Double.MIN_VALUE, 0.0) + ","
                + Double.compare(-1.0, Double.NEGATIVE_INFINITY);
    }

    static String min_max() {
        StringBuilder out = new StringBuilder();
        out.append("nan_min_d=")
                .append(Double.isNaN(Math.min(Double.NaN, 1.0))).append(',')
                .append(Double.isNaN(Math.min(1.0, Double.NaN))).append(',')
                .append(Double.isNaN(Math.max(Double.NaN, 1.0))).append(',')
                .append(Double.isNaN(Math.max(1.0, Double.NaN))).append('\n');
        out.append("nan_min_f=")
                .append(Float.isNaN(Math.min(Float.NaN, 1.0f))).append(',')
                .append(Float.isNaN(Math.min(1.0f, Float.NaN))).append(',')
                .append(Float.isNaN(Math.max(Float.NaN, 1.0f))).append(',')
                .append(Float.isNaN(Math.max(1.0f, Float.NaN))).append('\n');
        out.append("zero_max=")
                .append(hexDouble(Math.max(-0.0, 0.0))).append(',')
                .append(hexDouble(Math.max(0.0, -0.0))).append(',')
                .append(hexDouble(Math.max(-0.0, -0.0))).append(',')
                .append(hexDouble(Math.min(0.0, -0.0))).append(',')
                .append(hexDouble(Math.min(-0.0, 0.0))).append(',')
                .append(hexDouble(Math.min(0.0, 0.0))).append('\n');
        out.append("zero_max_f=")
                .append(hexFloat(Math.max(-0.0f, 0.0f))).append(',')
                .append(hexFloat(Math.max(0.0f, -0.0f))).append(',')
                .append(hexFloat(Math.min(0.0f, -0.0f))).append(',')
                .append(hexFloat(Math.min(-0.0f, 0.0f))).append('\n');
        out.append("ints=")
                .append(Math.min(1, 2)).append(',')
                .append(Math.max(-3, -7)).append(',')
                .append(Math.min(Integer.MIN_VALUE, 0)).append(',')
                .append(Math.max(Integer.MAX_VALUE, 0)).append(',')
                .append(Math.min(5L, 3L)).append(',')
                .append(Math.max(Long.MIN_VALUE, 0L)).append('\n');
        return out.toString();
    }

    static String abs_bounds() {
        return Math.abs(Integer.MIN_VALUE) + ","
                + Math.abs(Long.MIN_VALUE) + ","
                + Math.abs(-7) + ","
                + Math.abs(-7L) + ","
                + hexDouble(Math.abs(-0.0)) + ","
                + hexFloat(Math.abs(-0.0f)) + ","
                + hexDouble(Math.abs(Double.NEGATIVE_INFINITY));
    }

    static String nan_bits() {
        double payload = Double.longBitsToDouble(0x7ff8_0000_0000_1234L);
        float floatPayload = Float.intBitsToFloat(0x7f80_1234);
        return hex(Double.doubleToLongBits(Double.NaN)) + ","
                + hex(Double.doubleToLongBits(payload)) + ","
                + hex(Double.doubleToRawLongBits(payload)) + ","
                + Integer.toHexString(Float.floatToIntBits(floatPayload)) + ","
                + Integer.toHexString(Float.floatToRawIntBits(floatPayload)) + ","
                + hex(Double.doubleToLongBits(Double.POSITIVE_INFINITY)) + ","
                + hex(Double.doubleToLongBits(-0.0)) + ","
                + hex(Double.doubleToLongBits(Double.longBitsToDouble(0x7ff0_0000_0000_0001L)));
    }

    static String intTry(String text, int radix) {
        try {
            return Integer.toString(Integer.parseInt(text, radix));
        } catch (NumberFormatException e) {
            return e.getClass().getSimpleName();
        }
    }

    static String longTry(String text, int radix) {
        try {
            return Long.toString(Long.parseLong(text, radix));
        } catch (NumberFormatException e) {
            return e.getClass().getSimpleName();
        }
    }

    static String parse_int() {
        String[] forms = {"0", "+5", "-5", " 5", "5 ", "", "-", "+", "2147483647",
                "-2147483648", "2147483648", "-2147483649", "1_0", "0x10", "ff", "00ff",
                "z", "08", "\u00bd", "\u0663"};
        StringBuilder out = new StringBuilder();
        for (String form : forms) {
            out.append("pi[").append(form).append("]=")
                    .append(intTry(form, 10)).append('/')
                    .append(intTry(form, 16)).append('/')
                    .append(longTry(form, 10)).append('/')
                    .append(longTry(form, 16)).append('\n');
        }
        return out.toString();
    }

    static String parse_radix_bounds() {
        StringBuilder out = new StringBuilder();
        out.append("radix_low=").append(intTry("10", 1))
                .append(',').append(longTry("10", 0)).append('\n');
        out.append("radix_high=").append(intTry("10", 37))
                .append(',').append(longTry("10", 36)).append('\n');
        out.append("null_int=").append(intTry(null, 10)).append('\n');
        String nf = null;
        String nullDouble;
        try {
            nullDouble = Double.toString(Double.parseDouble(nf));
        } catch (NullPointerException e) {
            nullDouble = e.getClass().getSimpleName();
        }
        out.append("null_double=").append(nullDouble).append('\n');
        return out.toString();
    }

    static String doubleTry(String text) {
        try {
            return hex(Double.doubleToRawLongBits(Double.parseDouble(text)));
        } catch (NumberFormatException e) {
            return e.getClass().getSimpleName();
        }
    }

    static String floatTry(String text) {
        try {
            return Integer.toHexString(Float.floatToRawIntBits(Float.parseFloat(text)));
        } catch (NumberFormatException e) {
            return e.getClass().getSimpleName();
        }
    }

    static String parse_double() {
        String[] accepted = {"1.0", "1.0f", "1.0d", "1.0F", "+1.5", " -2.5 ", "\t3.5\n",
                "0x1p3", "0x1.8p1", "0x1p-2", "0X1P3", "1e5", "1E+5", ".5", "5.",
                "NaN", "+NaN", "-NaN", "Infinity", "-Infinity", "+Infinity", "-0.0", "1d"};
        String[] rejected = {"inf", "INF", "NAN", "nan", "Infinityf", "1e", "1.2.3", "1_0",
                "", " ", "0x1", "+-1", "--1", "0x", "1,0", "\u0001", "0x1p", "0x1p+"};
        StringBuilder out = new StringBuilder();
        for (String form : accepted) {
            out.append("pd[").append(form).append("]=")
                    .append(doubleTry(form)).append('/').append(floatTry(form)).append('\n');
        }
        for (String form : rejected) {
            out.append("pd_bad[").append(form).append("]=")
                    .append(doubleTry(form)).append('/').append(floatTry(form)).append('\n');
        }
        return out.toString();
    }

    static String integer_strings() {
        StringBuilder out = new StringBuilder();
        out.append("to_string=")
                .append(Integer.toString(255, 16)).append(',')
                .append(Integer.toString(-255, 16)).append(',')
                .append(Integer.toString(35, 36)).append(',')
                .append(Integer.toString(Integer.MIN_VALUE, 2)).append(',')
                .append(Integer.toString(0, 16)).append(',')
                .append(Long.toString(-1L, 36)).append(',')
                .append(Long.toString(0, 16)).append(',')
                .append(Long.toString(Long.MIN_VALUE, 2)).append('\n');
        out.append("hex=")
                .append(Integer.toHexString(-1)).append(',')
                .append(Integer.toHexString(255)).append(',')
                .append(Integer.toBinaryString(5)).append(',')
                .append(Integer.toBinaryString(-1)).append(',')
                .append(Integer.toOctalString(8)).append(',')
                .append(Integer.toOctalString(-1)).append(',')
                .append(Long.toHexString(Long.MIN_VALUE)).append(',')
                .append(Long.toHexString(-1L)).append(',')
                .append(Long.toBinaryString(1L)).append(',')
                .append(Long.toOctalString(-1L)).append('\n');
        out.append("unsigned=")
                .append(Integer.toUnsignedString(-1)).append(',')
                .append(Integer.toUnsignedString(-1, 16)).append(',')
                .append(Integer.toUnsignedString(Integer.MIN_VALUE)).append(',')
                .append(Long.toUnsignedString(-1L)).append(',')
                .append(Long.toUnsignedString(-1L, 36)).append(',')
                .append(Long.toUnsignedString(Long.MIN_VALUE)) .append('\n');
        out.append("unsigned_math=")
                .append(Integer.divideUnsigned(-1, 2)).append(',')
                .append(Integer.remainderUnsigned(-1, 3)).append(',')
                .append(Integer.divideUnsigned(7, 2)).append(',')
                .append(Long.divideUnsigned(-1L, 3L)).append(',')
                .append(Long.remainderUnsigned(-1L, 10L)).append(',')
                .append(Integer.compareUnsigned(-1, 1)).append(',')
                .append(Long.compareUnsigned(-1L, 1L)).append('\n');
        out.append("parse_unsigned=")
                .append(Integer.parseUnsignedInt("4294967295")).append(',')
                .append(Integer.parseUnsignedInt("ffffffff", 16)).append(',')
                .append(Long.parseUnsignedLong("18446744073709551615")).append(',')
                .append(Long.parseUnsignedLong("zz", 36)).append(',')
                .append(Integer.parseUnsignedInt("0")).append('\n');
        out.append("parse_unsigned_bad=");
        try {
            out.append(Integer.parseUnsignedInt("-1"));
        } catch (NumberFormatException e) {
            out.append(e.getClass().getSimpleName());
        }
        out.append(',');
        try {
            out.append(Long.parseUnsignedLong("18446744073709551616"));
        } catch (NumberFormatException e) {
            out.append(e.getClass().getSimpleName());
        }
        out.append('\n');
        out.append("value_of=")
                .append(Integer.valueOf(5)).append(',')
                .append(Integer.valueOf("5")).append(',')
                .append(Integer.valueOf("ff", 16)).append(',')
                .append(Long.valueOf("5")).append(',')
                .append(Long.valueOf("ff", 16)).append(',')
                .append(Integer.parseInt("5")).append(',')
                .append(Double.valueOf("1.5")).append('\n');
        return out.toString();
    }

    static String compare_helpers() {
        return Double.compare(0.0, 1.0) + ","
                + Double.compare(2.0, 1.0) + ","
                + Double.compare(1.0, 1.0) + ","
                + Float.compare(2.0f, 1.0f) + ","
                + Integer.compare(1, 2) + ","
                + Integer.compare(-1, -2) + ","
                + Long.compare(2L, 1L) + ","
                + Integer.signum(-5) + ","
                + Long.signum(0L);
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("nan_compare=").append(nan_compare()).append('\n');
        out.append("zero_compare=").append(zero_compare()).append('\n');
        out.append("min_max=").append(min_max());
        out.append("abs_bounds=").append(abs_bounds()).append('\n');
        out.append("nan_bits=").append(nan_bits()).append('\n');
        out.append("parse_int=").append(parse_int());
        out.append("parse_radix_bounds=").append(parse_radix_bounds());
        out.append("parse_double=").append(parse_double());
        out.append("integer_strings=").append(integer_strings());
        out.append("compare_helpers=").append(compare_helpers()).append('\n');
        return out.toString();
    }
}
