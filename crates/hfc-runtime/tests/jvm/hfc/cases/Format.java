package hfc.cases;

import java.util.IllegalFormatException;

/**
 * {@code java.util.Formatter} behaviour: the conversions a mod uses, the flags
 * and padding rules, argument indexing, and the exceptions a malformed or
 * ill-typed specifier raises.
 *
 * Every line's label is a plain identifier the differential harness compares
 * verbatim, so a padding, rounding or exception difference shows up on its own
 * line.
 */
public final class Format {
    /** Format with the default locale, reporting an {@code IllegalFormatException}
     *  by class name and message (which is what a mod's own error path sees). */
    static String f(String format, Object... args) {
        try {
            return String.format(format, args);
        } catch (IllegalFormatException e) {
            return "THREW " + e.getClass().getSimpleName() + ": " + e.getMessage();
        }
    }

    /** A format result with its quotes and newlines made visible, so padding and
     *  line breaks are compared rather than lost in the surrounding text. */
    static String shown(String format, Object... args) {
        return "[" + f(format, args).replace("\n", "\\n") + "]";
    }

    static String conversions() {
        StringBuilder out = new StringBuilder();
        out.append("string=").append(shown("%s|%S", "MiXeD", "MiXeD")).append('\n');
        out.append("string_null=").append(shown("%s", (Object) null)).append('\n');
        out.append("string_object=").append(shown("%s", 3.0)).append('\n');
        out.append("decimal=").append(shown("%d|%d", 0, -2147483648)).append('\n');
        out.append("long=").append(shown("%d", -9223372036854775808L)).append('\n');
        out.append("hex=").append(shown("%x|%X", 255, 255)).append('\n');
        out.append("hex_negative=").append(shown("%x|%X", -1, -1)).append('\n');
        out.append("hex_long=").append(shown("%x", -1L)).append('\n');
        out.append("octal=").append(shown("%o|%o", 8, -1)).append('\n');
        out.append("float=").append(shown("%f", 3.5)).append('\n');
        out.append("float_default_precision=").append(shown("%f", 1.0 / 3.0)).append('\n');
        out.append("float_float_arg=").append(shown("%f", 2.5f)).append('\n');
        out.append("scientific=").append(shown("%e|%E", 1234.5678, 1234.5678)).append('\n');
        out.append("general=").append(shown("%g|%G", 0.0001, 1e7)).append('\n');
        out.append("char=").append(shown("%c|%C", 'q', 'q')).append('\n');
        out.append("char_codepoint=").append(shown("%c", 65)).append('\n');
        out.append("boolean=").append(shown("%b|%B|%b", true, false, (Object) null)).append('\n');
        out.append("boolean_not_boolean=").append(shown("%b|%b", "false", 0)).append('\n');
        out.append("hash=").append(shown("%h", 255)).append('\n');
        out.append("hex_float=").append(shown("%a", 1.0)).append('\n');
        return out.toString();
    }

    static String widths() {
        StringBuilder out = new StringBuilder();
        out.append("width=").append(shown("%13s|", "abc")).append('\n');
        out.append("left_align=").append(shown("%-13s|", "abc")).append('\n');
        out.append("width_number=").append(shown("%10d|", 42)).append('\n');
        out.append("width_null=").append(shown("%10s|", (Object) null)).append('\n');
        out.append("zero_pad=").append(shown("%05d|", 42)).append('\n');
        out.append("zero_pad_negative=").append(shown("%05d|", -42)).append('\n');
        out.append("zero_pad_sign=").append(shown("%+05d|", 42)).append('\n');
        out.append("zero_pad_parentheses=").append(shown("%(05d|", -42)).append('\n');
        out.append("zero_pad_float=").append(shown("%08.2f|", -3.14159)).append('\n');
        out.append("zero_pad_hex=").append(shown("%#08x|", 255)).append('\n');
        out.append("width_no_space=").append(shown("%3s|", "abcdef")).append('\n');
        out.append("left_align_float=").append(shown("%-12.2f|", 1.5)).append('\n');
        return out.toString();
    }

    static String precision() {
        StringBuilder out = new StringBuilder();
        out.append("precision_float=").append(shown("%.2f|%.0f|%.4f", 2.675, 2.5, 1.0 / 3.0)).append('\n');
        out.append("precision_string=").append(shown("%.2s|%.0s|", "abcdef", "abcdef")).append('\n');
        out.append("precision_string_width=").append(shown("%10.2s|", "abcdef")).append('\n');
        out.append("precision_rounding=").append(shown("%.1f|%.2f|%.2f", 2.25, 2.345, 1.005)).append('\n');
        out.append("precision_e=").append(shown("%.2e", 1234.5678)).append('\n');
        out.append("precision_g=").append(shown("%.3g|%.1g", 1234567.0, 1234.0)).append('\n');
        return out.toString();
    }

    static String grouping_and_flags() {
        StringBuilder out = new StringBuilder();
        out.append("group=").append(shown("%,d|%,d", 1234567, -1234567)).append('\n');
        out.append("group_long=").append(shown("%,d", 9223372036854775807L)).append('\n');
        out.append("group_float=").append(shown("%,.2f", 1234567.891)).append('\n');
        out.append("group_zero_pad=").append(shown("%,010d|%,012.2f", 1234, 1234.5)).append('\n');
        out.append("parentheses=").append(shown("%(d|%(d", -5, 5)).append('\n');
        out.append("parentheses_group=").append(shown("%(,d", -1234567)).append('\n');
        out.append("plus=").append(shown("%+d|%+d", 5, -5)).append('\n');
        out.append("leading_space=").append(shown("% d|% d", 5, -5)).append('\n');
        out.append("alternate_hex=").append(shown("%#x|%#X|%#o", 255, 255, 8)).append('\n');
        out.append("alternate_hex_zero=").append(shown("%#x|%#o", 0, 0)).append('\n');
        out.append("alternate_float=").append(shown("%#.0f|%#.0e", 1.5, 1.5)).append('\n');
        return out.toString();
    }

    static String special_values() {
        StringBuilder out = new StringBuilder();
        out.append("nan=").append(shown("%f|%e|%g", Double.NaN, Double.NaN, Double.NaN)).append('\n');
        out.append("infinity=").append(shown("%f|%f", Double.POSITIVE_INFINITY, Double.NEGATIVE_INFINITY)).append('\n');
        out.append("nan_width=").append(shown("%8f|", Double.NaN)).append('\n');
        out.append("signed_zero=").append(shown("%f|%+.2f|%d", -0.0, -0.0, 0)).append('\n');
        out.append("negative_zero_round=").append(shown("%.2f", -0.0)).append('\n');
        out.append("tiny=").append(shown("%.20f", 1e-10)).append('\n');
        out.append("huge=").append(shown("%.1f", 1e20)).append('\n');
        out.append("max_double=").append(shown("%e", Double.MAX_VALUE)).append('\n');
        return out.toString();
    }

    static String argument_index() {
        StringBuilder out = new StringBuilder();
        out.append("index=").append(shown("%2$s-%1$s", "a", "b")).append('\n');
        out.append("index_repeat=").append(shown("%1$s %1$s", "a")).append('\n');
        out.append("index_mixed=").append(shown("%1$s %s %s", "a", "b")).append('\n');
        out.append("previous=").append(shown("%s %<s", "a", "b")).append('\n');
        out.append("previous_indexed=").append(shown("%2$s %<s", "a", "b")).append('\n');
        out.append("index_width=").append(shown("%1$05d", 42)).append('\n');
        out.append("index_missing=").append(shown("%3$s", "a", "b")).append('\n');
        out.append("index_zero=").append(shown("%0$s", "a")).append('\n');
        out.append("index_previous_first=").append(shown("%<s", "a")).append('\n');
        return out.toString();
    }

    static String literals() {
        StringBuilder out = new StringBuilder();
        out.append("percent=").append(shown("%d%%", 50)).append('\n');
        out.append("percent_alone=").append(shown("%%")).append('\n');
        out.append("newline=").append(shown("a%nb")).append('\n');
        out.append("newline_only=").append(shown("%n")).append('\n');
        out.append("literal_text=").append(shown("no specifiers")).append('\n');
        out.append("empty=").append(shown("")).append('\n');
        return out.toString();
    }

    /** The padding idiom a centring mixin uses: a format string built at run time
     *  to a given width, formatting the empty string. */
    static String padding_idiom() {
        StringBuilder out = new StringBuilder();
        for (int width : new int[] {1, 4, 8, 13, 40}) {
            String spaces = String.format("%" + width + "s", "");
            out.append("pad_").append(width).append("=[").append(spaces)
                    .append("] len=").append(spaces.length()).append('\n');
        }
        // The title shape the tooltip path builds: text centred in a field.
        String title = "Diamond Sword";
        int total = 20;
        int pad = total - title.length();
        String left = String.format("%" + (pad / 2) + "s", "");
        String right = String.format("%" + (pad - pad / 2) + "s", "");
        out.append("centred=[").append(left).append(title).append(right).append("]\n");
        return out.toString();
    }

    static String errors() {
        StringBuilder out = new StringBuilder();
        out.append("unknown_conversion=").append(shown("%q", 1)).append('\n');
        out.append("unknown_uppercase=").append(shown("%Q", 1)).append('\n');
        out.append("trailing_percent=").append(shown("abc%", 1)).append('\n');
        out.append("missing_conversion=").append(shown("%"))
                .append('\n');
        out.append("duplicate_flag=").append(shown("%--5d", 1)).append('\n');
        out.append("flags_mismatch_plus=").append(shown("%+s", "x")).append('\n');
        out.append("flags_mismatch_zero=").append(shown("%0s", "x")).append('\n');
        out.append("flags_mismatch_hex=").append(shown("%+x", 1)).append('\n');
        out.append("flags_mismatch_group=").append(shown("%,x", 1)).append('\n');
        out.append("flags_conflict=").append(shown("%-05d", 1)).append('\n');
        out.append("flags_conflict_signs=").append(shown("%+ d", 1)).append('\n');
        out.append("missing_width=").append(shown("%-d", 1)).append('\n');
        out.append("missing_width_zero=").append(shown("%0d", 1)).append('\n');
        out.append("conversion_mismatch_d=").append(shown("%d", 3.5)).append('\n');
        out.append("conversion_mismatch_f=").append(shown("%f", 5)).append('\n');
        out.append("conversion_mismatch_c=").append(shown("%c", 1L)).append('\n');
        out.append("precision_on_integer=").append(shown("%.2d", 5)).append('\n');
        out.append("bad_code_point=").append(shown("%c", -1)).append('\n');
        out.append("missing_argument=").append(shown("%s %s", "a")).append('\n');
        out.append("no_specifier_for_width=").append(shown("%012,d", 1234)).append('\n');
        return out.toString();
    }


    /** Rounding parity with the JDK's `FormattedFPDecimal`: ties go toward
     *  positive infinity, and the value is the decimal `Double.toString`
     *  selects, not a naive shortest-rounding of the binary fraction. */
    static String rounding() {
        double[] values = {0.145, 1.005, 2.675, 2.345, 2.355, 1.115, 8.835, 0.615,
                9.995, 0.5, 1.5, 2.5, 3.5, 0.05, 0.15, 0.25, 0.35, 1.0 / 3.0};
        StringBuilder out = new StringBuilder();
        for (double value : values) {
            out.append("round_").append(Double.toString(value)).append('=')
                    .append(f("%.1f|%.2f|%.2e|%.2g|%.3g", value, value, value, value, value))
                    .append('\n');
        }
        // `%g` form selection is decided by the *rounded* exponent.
        double[] general = {0.0, -0.0, 1.0, 9.99, 0.0999, 99.9, 999.9, 9999.9,
                1e-4, 1e-5, 123456.0, 1234567.0, 1e7, 1e8, 100.0, 0.001, 0.01, 123.456};
        for (double value : general) {
            for (int precision : new int[] {1, 2, 3, 6}) {
                out.append("g_").append(Double.toString(value)).append('_').append(precision)
                        .append('=').append(f("%." + precision + "g", value)).append('\n');
            }
        }
        return out.toString();
    }

    /** A float argument is widened before the conversion, which changes the
     *  digits a precision exposes. */
    static String float_precision() {
        StringBuilder out = new StringBuilder();
        out.append("float_exact=").append(f("%.20f|%.20e", 0.1f, 0.1f)).append('\n');
        out.append("double_exact=").append(f("%.20f|%.20e", 0.1, 0.1)).append('\n');
        out.append("float_g=").append(f("%.20g", 0.1f)).append('\n');
        return out.toString();
    }


    /** `String.valueOf` and `String.format("%s")` both reach the argument's own
     *  `toString`, so the numeric wrappers must spell their values the JDK way. */
    static String value_of() {
        StringBuilder out = new StringBuilder();
        out.append("valueof_double=").append(String.valueOf(3.0))
                .append('|').append(String.valueOf(1e7))
                .append('|').append(String.valueOf(1e-4)).append('\n');
        out.append("valueof_float=").append(String.valueOf(3.0f))
                .append('|').append(String.valueOf(1e8f)).append('\n');
        out.append("valueof_int=").append(String.valueOf(42))
                .append('|').append(String.valueOf('x'))
                .append('|').append(String.valueOf(true))
                .append('|').append(String.valueOf((Object) null)).append('\n');
        out.append("valueof_long=").append(String.valueOf(1234567890123L)).append('\n');
        out.append("concat_double=").append("" + 3.0 + "|" + 1e7 + "|" + 1e-4).append('\n');
        return out.toString();
    }

    /** `NumberFormat.getInstance()` groups in threes and rejects a non-Number. */
    static String number_format() {
        java.text.NumberFormat format = java.text.NumberFormat.getInstance();
        StringBuilder out = new StringBuilder();
        Object[] values = {1234567, 1234567890123L, 1234.5, 1234.5678, 1234.5674,
                0.0001, 0.0, -0.0, Double.NaN, Double.POSITIVE_INFINITY, null, "x"};
        for (Object value : values) {
            String label = value == null ? "null" : value.getClass().getSimpleName();
            try {
                out.append("nf_").append(label).append('=')
                        .append('[').append(format.format(value)).append(']').append('\n');
            } catch (IllegalArgumentException e) {
                out.append("nf_").append(label).append("=THREW ")
                        .append(e.getClass().getSimpleName()).append('\n');
            }
        }
        return out.toString();
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("conversions=").append('\n').append(conversions());
        out.append("widths=").append('\n').append(widths());
        out.append("precision=").append('\n').append(precision());
        out.append("grouping=").append('\n').append(grouping_and_flags());
        out.append("special=").append('\n').append(special_values());
        out.append("indexing=").append('\n').append(argument_index());
        out.append("literals=").append('\n').append(literals());
        out.append("padding=").append('\n').append(padding_idiom());
        out.append("errors=").append('\n').append(errors());
        out.append("rounding=").append('\n').append(rounding());
        out.append("float_precision=").append('\n').append(float_precision());
        out.append("value_of=").append('\n').append(value_of());
        out.append("number_format=").append('\n').append(number_format());
        return out.toString();
    }
}
