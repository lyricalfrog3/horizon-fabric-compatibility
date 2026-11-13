package hfc.cases;

/**
 * Java's `String` is UTF-16 code units and `Character`'s predicates follow
 * Unicode general categories, not Rust's `char` properties. These lines pin
 * length/charAt/substring/indexOf/codePoint indexing over an astral character,
 * the exceptions an out-of-range index throws, string identity (only literals
 * and `intern()` are shared), and the category-driven `Character` predicates.
 */
public final class Unicode {
    /** "a", U+1F600, "b": four UTF-16 code units, three code points. */
    static final String ASTRAL = "a\uD83D\uDE00b";

    static String length_charat() {
        StringBuilder out = new StringBuilder();
        out.append("length=").append(ASTRAL.length()).append(',')
                .append("empty=").append("".length()).append(',')
                .append("bmp=").append("\u00e9\u4e16".length()).append('\n');
        out.append("char_at=")
                .append(Integer.toHexString(ASTRAL.charAt(0))).append(',')
                .append(Integer.toHexString(ASTRAL.charAt(1))).append(',')
                .append(Integer.toHexString(ASTRAL.charAt(2))).append(',')
                .append(Integer.toHexString(ASTRAL.charAt(3))).append('\n');
        out.append("char_cast=")
                .append((int) ASTRAL.charAt(1) > 0xffff ? "wide" : "unit").append(',')
                .append((int) ASTRAL.charAt(1)).append(',')
                .append((int) ASTRAL.charAt(2)).append('\n');
        return out.toString();
    }

    static String substrings() {
        StringBuilder out = new StringBuilder();
        // Boundaries are code units. A substring that covers a whole surrogate
        // pair keeps its characters; one that would end inside a pair is not
        // exercised here, because the interpreter cannot store a lone
        // surrogate (see its `String` note) and this suite compares text.
        out.append("sub_0_2=").append(ASTRAL.substring(0, 2).length()).append(',')
                .append(Integer.toHexString(ASTRAL.substring(0, 2).codePointAt(0))).append('\n');
        out.append("sub_3_4=").append(ASTRAL.substring(3, 4).length()).append(',')
                .append(ASTRAL.substring(3, 4)).append('\n');
        out.append("sub_0_3=").append(ASTRAL.substring(0, 3).length()).append(',')
                .append(ASTRAL.substring(0, 3).codePointCount(0, 3)).append('\n');
        out.append("sub_2=").append(ASTRAL.substring(2).length()).append(',')
                .append(ASTRAL.substring(2).equals("\uD83D\uDE00b")).append('\n');
        out.append("sub_1_3_cp=").append(ASTRAL.substring(1, 3).length()).append(',')
                .append(Integer.toHexString(ASTRAL.substring(1, 3).codePointAt(0))).append('\n');
        out.append("empty_sub=").append(ASTRAL.substring(2, 2).length()).append(',')
                .append(ASTRAL.substring(ASTRAL.length()).length()).append('\n');
        return out.toString();
    }

    static String index_of() {
        StringBuilder out = new StringBuilder();
        out.append("index_of=")
                .append(ASTRAL.indexOf("b")).append(',')
                .append(ASTRAL.indexOf("\uD83D\uDE00")).append(',')
                .append(ASTRAL.indexOf(0x1F600)).append(',')
                .append(ASTRAL.lastIndexOf(0x1F600)).append(',')
                .append(ASTRAL.indexOf("a")).append(',')
                .append(ASTRAL.indexOf("z")).append('\n');
        // The code point and its unit pair start at the same index.
        out.append("index_of_cp=")
                .append(ASTRAL.indexOf(0x1F600)).append(',')
                .append(ASTRAL.indexOf(0xD83D)).append(',')
                .append(ASTRAL.indexOf(0xDE00)).append(',')
                .append(ASTRAL.lastIndexOf(0xDE00)).append('\n');
        out.append("code_points=")
                .append(ASTRAL.codePointAt(0)).append(',')
                .append(ASTRAL.codePointAt(1)).append(',')
                .append(ASTRAL.codePointAt(2)).append(',')
                .append(ASTRAL.codePointAt(3)).append(',')
                .append(ASTRAL.codePointBefore(1)).append(',')
                .append(ASTRAL.codePointBefore(3)).append(',')
                .append(ASTRAL.codePointCount(0, ASTRAL.length())).append(',')
                .append(ASTRAL.offsetByCodePoints(0, 2)).append('\n');
        return out.toString();
    }

    static String bounds() {
        StringBuilder out = new StringBuilder();
        try {
            ASTRAL.charAt(4);
            out.append("char_at_oob=none");
        } catch (StringIndexOutOfBoundsException e) {
            out.append("char_at_oob=StringIndexOutOfBoundsException");
        }
        try {
            ASTRAL.charAt(-1);
            out.append(",neg_char_at=none");
        } catch (StringIndexOutOfBoundsException e) {
            out.append(",neg_char_at=StringIndexOutOfBoundsException");
        }
        try {
            ASTRAL.substring(2, 1);
            out.append(",sub_reversed=none");
        } catch (StringIndexOutOfBoundsException e) {
            out.append(",sub_reversed=StringIndexOutOfBoundsException");
        }
        try {
            ASTRAL.substring(5);
            out.append(",sub_past_end=none");
        } catch (StringIndexOutOfBoundsException e) {
            out.append(",sub_past_end=StringIndexOutOfBoundsException");
        }
        try {
            out.append(",code_point_at_oob=").append(ASTRAL.codePointAt(4));
        } catch (StringIndexOutOfBoundsException e) {
            out.append(",code_point_at_oob=StringIndexOutOfBoundsException");
        }
        out.append('\n');
        return out.toString();
    }

    static String identity() {
        StringBuilder builder = new StringBuilder("x");
        String first = builder.toString();
        String second = builder.toString();
        String literal = "x";
        StringBuilder out = new StringBuilder();
        out.append("builder_distinct=").append(first == second)
                .append(',').append(first == literal)
                .append(',').append(first.equals(literal)).append('\n');
        String padded = "  hi  ";
        String trimmed = "hi";
        out.append("trim_distinct=").append(padded.trim() == padded)
                .append(',').append(trimmed.trim() == trimmed)
                .append(',').append(padded.strip() == padded)
                .append(',').append(trimmed.strip() == trimmed).append('\n');
        out.append("casing=").append(trimmed.toUpperCase() == "HI")
                .append(',').append("ABC".toUpperCase() == "ABC")
                .append(',').append(trimmed.toLowerCase() == trimmed)
                .append(',').append("xY".toLowerCase() == "xy").append('\n');
        out.append("concat_replace=").append(trimmed.concat("") == trimmed)
                .append(',').append("abc".replace('x', 'y') == "abc")
                .append(',').append("abc".substring(0) == "abc")
                .append(',').append("abc".substring(0, 3) == "abc").append('\n');
        out.append("value_of=").append(String.valueOf((Object) trimmed) == trimmed)
                .append(',').append(String.valueOf(trimmed) == trimmed)
                .append(',').append(String.valueOf(5) == "5").append('\n');
        out.append("literals=").append("ab" == "ab")
                .append(',').append(("a" + "b") == "ab")
                .append(',').append(new String("hello") == "hello")
                .append(',').append(new String("hello").intern() == "hello").append('\n');
        return out.toString();
    }

    static String char_predicates() {
        int[] units = {'\u00bd', '\u0662', '\u2160', '\u216b', '1', 'a', '\u00e9', '\u0301',
                '\u207f', '\uff11', '\u0660'};
        StringBuilder out = new StringBuilder();
        for (int unit : units) {
            out.append("char[U+").append(Integer.toHexString(unit).toUpperCase()).append("]=")
                    .append(Character.isDigit(unit)).append(',')
                    .append(Character.isLetter(unit)).append(',')
                    .append(Character.isLetterOrDigit(unit)).append(',')
                    .append(Character.getNumericValue(unit)).append('\n');
        }
        return out.toString();
    }

    static String space_characters() {
        int[] units = {'\t', '\u000b', '\u001c', '\u001f', ' ', '\u00a0', '\u2007', '\u202f',
                '\u2000', '\u2028', '\u3000', '\u200b', '\u180e', '\u0085', '\u0000'};
        StringBuilder out = new StringBuilder();
        for (int unit : units) {
            out.append("space[U+").append(Integer.toHexString(unit).toUpperCase()).append("]=")
                    .append(Character.isWhitespace(unit)).append(',')
                    .append(Character.isSpaceChar(unit)).append(',')
                    .append(Character.isLetterOrDigit(unit)).append('\n');
        }
        return out.toString();
    }

    static String digit_helpers() {
        StringBuilder out = new StringBuilder();
        out.append("digit=")
                .append(Character.digit('f', 16)).append(',')
                .append(Character.digit('z', 10)).append(',')
                .append(Character.digit('\u0662', 10)).append(',')
                .append(Character.digit('\u0665', 10)).append(',')
                .append(Character.digit('5', 37)).append(',')
                .append(Character.digit('5', 1)).append('\n');
        out.append("for_digit=")
                .append(Character.forDigit(10, 16)).append(',')
                .append((int) Character.forDigit(36, 36)).append(',')
                .append((int) Character.forDigit(-1, 10)).append(',').append((int) Character.forDigit(5, 1)).append('\n');
        out.append("unicode_digit_parse=")
                .append(parseOrThrow("\u0663\u0664")).append(',')
                .append(parseOrThrow("\uFF11\uFF12")).append('\n');
        return out.toString();
    }

    static String parseOrThrow(String text) {
        try {
            return Integer.toString(Integer.parseInt(text));
        } catch (NumberFormatException e) {
            return e.getClass().getSimpleName();
        }
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("length_charat=").append(length_charat());
        out.append("substrings=").append(substrings());
        out.append("index_of=").append(index_of());
        out.append("bounds=").append(bounds());
        out.append("identity=").append(identity());
        out.append("char_predicates=").append(char_predicates());
        out.append("space_characters=").append(space_characters());
        out.append("digit_helpers=").append(digit_helpers());
        return out.toString();
    }
}
