package hfc.cases;

/**
 * `Character.toChars(int)` answers a `char[]`, not an `int`: one unit for a BMP
 * code point and the surrogate pair for a supplementary one, and it throws
 * `IllegalArgumentException` for a value that is not a code point at all.
 *
 * This is the shape Iceberg's `StringRecomposer.RecomposerSink` uses to rebuild
 * a tooltip line — `StringBuilder.append(Character.toChars(codePoint))` — so a
 * `toChars` that answered the code point itself made that append a no-op and
 * left the rebuilt title as empty text.
 */
public final class CharacterArrays {

    static String bmp_array() {
        char[] one = Character.toChars('A');
        StringBuilder out = new StringBuilder();
        out.append("len=").append(one.length).append(',')
                .append("value=").append(one[0]).append(',')
                .append("unit=").append((int) one[0]).append('\n');
        return out.toString();
    }

    static String supplementary_array() {
        char[] pair = Character.toChars(0x1F600);
        StringBuilder out = new StringBuilder();
        out.append("len=").append(pair.length).append(',')
                .append("high=").append(Integer.toHexString(pair[0])).append(',')
                .append("low=").append(Integer.toHexString(pair[1])).append(',')
                .append("round_trip=")
                .append(Integer.toHexString(Character.toCodePoint(pair[0], pair[1])))
                .append('\n');
        return out.toString();
    }

    static String char_count() {
        return "bmp=" + Character.charCount('A')
                + ",supplementary=" + Character.charCount(0x1F600) + '\n';
    }

    /** The append that Iceberg's recomposer performs, one code point at a time. */
    static String append_to_builder() {
        int[] codePoints = {'A', 'B', 0x1F600, 'C'};
        StringBuilder builder = new StringBuilder();
        for (int codePoint : codePoints) {
            builder.append(Character.toChars(codePoint));
        }
        String built = builder.toString();
        return "built=" + built
                + ",units=" + built.length()
                + ",points=" + built.codePointCount(0, built.length()) + '\n';
    }

    /**
     * A rebuild in the recomposer's own shape: append each code point to a
     * builder, flush the accumulated text into a component whenever the style
     * changes, then clear the builder. A `toChars` that answers an `int` appends
     * nothing, so the flushed text is empty.
     */
    static String recompose_shaped() {
        String line = "  Space  ";
        StringBuilder builder = new StringBuilder();
        StringBuilder flushed = new StringBuilder();
        for (int i = 0; i < line.length(); i++) {
            builder.append(Character.toChars(line.charAt(i)));
            if (i == 3) {
                flushed.append(builder.toString());
                builder.setLength(0);
            }
        }
        flushed.append(builder.toString());
        return "flushed=[" + flushed + "],matches=" + flushed.toString().equals(line) + '\n';
    }

    /**
     * A surrogate is a legal BMP value and `toChars` answers it unchanged; only
     * a value outside 0..=0x10FFFF is not a code point at all, and that is what
     * the JDK throws for.
     */
    static String invalid_code_point() {
        StringBuilder out = new StringBuilder();
        int[] values = {0xD800, 0xDFFF, -1, 0x110000};
        for (int value : values) {
            try {
                char[] chars = Character.toChars(value);
                out.append("no_throw:").append(Integer.toHexString(value))
                        .append(",len=").append(chars.length);
            } catch (IllegalArgumentException e) {
                out.append("threw:").append(Integer.toHexString(value));
            }
            out.append('\n');
        }
        return out.toString();
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("bmp_array=").append(bmp_array());
        out.append("supplementary_array=").append(supplementary_array());
        out.append("char_count=").append(char_count());
        out.append("append_to_builder=").append(append_to_builder());
        out.append("recompose_shaped=").append(recompose_shaped());
        out.append("invalid_code_point=").append(invalid_code_point());
        return out.toString();
    }
}
