package hfc.cases;

/**
 * Opcode-level semantics the interpreter must match: float remainder, the
 * wrapping integer negations, and array type tests/casts.
 *
 * Every label here is compared against the real JVM by {@code jvm_diff.rs}; a
 * divergence is a runtime bug, never a test to relax.
 */
public final class Opcode {
    /** `frem` with ordinary operands: 7 % 2 is 1. */
    static float fremPlain(float a, float b) {
        return a % b;
    }

    /** Negative operands keep the dividend's sign, as Java's `%` does. */
    static float fremNegative(float a, float b) {
        return a % b;
    }

    /** A NaN operand answers NaN, and so does an infinite dividend or a zero divisor. */
    static float fremNaN(float a, float b) {
        return a % b;
    }

    /** A finite dividend modulo an infinite divisor is the dividend itself. */
    static float fremInfiniteDivisor(float a, float b) {
        return a % b;
    }

    /** `-0.0 % x` keeps the negative sign; `x % -0.0` is NaN. */
    static float fremZero(float a, float b) {
        return a % b;
    }

    /** `ineg` of `Integer.MIN_VALUE` wraps to itself, as the JVM's `ineg` does. */
    static int negateInt(int value) {
        return -value;
    }

    /** `lneg` of `Long.MIN_VALUE` wraps to itself. */
    static long negateLong(long value) {
        return -value;
    }

    /** The enum's own constants, for the switch below. */
    enum Season {
        WINTER, SPRING, SUMMER, AUTUMN, MONSOON, DRY
    }

    /**
     * A classic (non-pattern) enum switch. javac builds a synthetic
     * `$SwitchMap$…` array in this class's initializer, sized by
     * `Season.values().length` and indexed by each constant's ordinal, so
     * asking for six constants with a non-declaration order exercises a
     * switch map that a fabricated four-element identity array gets wrong.
     */
    static String enumSwitch(Season season) {
        switch (season) {
            case DRY:
                return "dry";
            case SUMMER:
                return "summer";
            case WINTER:
                return "winter";
            case MONSOON:
                return "monsoon";
            default:
                return "other";
        }
    }

    /** `instanceof` between array types compares components, not just "[". */
    static String arrayInstanceOf() {
        Object ints = new int[2];
        Object strings = new String[2];
        Object objects = new Object[2];
        Object matrix = new int[2][2];
        return (ints instanceof int[]) + ","
                + (ints instanceof String[]) + ","
                + (ints instanceof Object[]) + ","
                + (strings instanceof Object[]) + ","
                + (objects instanceof String[]) + ","
                + (matrix instanceof int[][]) + ","
                + (matrix instanceof Object[]) + ","
                + (matrix instanceof int[]);
    }

    /** Arrays implement `Cloneable` and `Serializable` whatever their component. */
    static String arraySupertypes() {
        Object ints = new int[1];
        Object strings = new String[1];
        return (ints instanceof Cloneable) + "," + (strings instanceof Cloneable) + ","
                + (ints instanceof java.io.Serializable) + "," + (strings instanceof java.io.Serializable);
    }

    /** A cross-array cast throws `ClassCastException`. */
    static String arrayCast() {
        Object strings = new String[] {"a"};
        StringBuilder out = new StringBuilder();
        try {
            int[] bad = (int[]) strings;
            out.append("no-throw:").append(bad.length);
        } catch (ClassCastException expected) {
            out.append("ClassCastException");
        }
        try {
            Object[] widened = (Object[]) strings;
            out.append(',').append("ok:").append(widened.length);
        } catch (ClassCastException wrong) {
            out.append(',').append("wrong:").append(wrong.getMessage());
        }
        return out.toString();
    }

    /** `Float.NaN % 1` and the other NaN combinations, as raw bit patterns. */
    static String floatNaNModulo() {
        return Float.floatToIntBits(Float.NaN % 1) + ","
                + Float.floatToIntBits(1 % Float.NaN) + ","
                + Float.floatToIntBits(Float.POSITIVE_INFINITY % Float.NaN);
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        // Every float result is compared by its bits, not by `Float.toString`:
        // Java's shortest float formatting is a separately tracked divergence
        // (`Arith.double_str`), and comparing text here would report it as a
        // `frem` bug.
        out.append("frem_plain=").append(Float.floatToIntBits(fremPlain(7.0f, 2.0f))).append('\n');
        out.append("frem_negative=").append(Float.floatToIntBits(fremNegative(-7.0f, 2.0f)))
                .append(',').append(Float.floatToIntBits(fremNegative(7.0f, -2.0f)))
                .append(',').append(Float.floatToIntBits(fremNegative(-7.0f, -2.0f))).append('\n');
        out.append("frem_nan=").append(Float.isNaN(fremNaN(Float.NaN, 1.0f))).append(',')
                .append(Float.isNaN(fremNaN(1.0f, Float.NaN))).append(',')
                .append(Float.isNaN(fremNaN(Float.POSITIVE_INFINITY, 2.0f))).append(',')
                .append(Float.isNaN(fremNaN(2.0f, 0.0f))).append(',')
                .append(Float.isNaN(fremNaN(Float.NEGATIVE_INFINITY, Float.POSITIVE_INFINITY)))
                .append('\n');
        out.append("frem_infinite_divisor=")
                .append(Float.floatToIntBits(fremInfiniteDivisor(7.5f, Float.POSITIVE_INFINITY)))
                .append(',').append(Float.floatToIntBits(fremInfiniteDivisor(-7.5f, Float.NEGATIVE_INFINITY)))
                .append('\n');
        out.append("frem_zero=").append(Float.floatToIntBits(fremZero(-0.0f, 3.0f))).append(',')
                .append(Float.isNaN(fremZero(3.0f, -0.0f))).append(',')
                .append(Float.floatToIntBits(fremZero(0.0f, 3.0f))).append('\n');
        out.append("float_nan_modulo=").append(floatNaNModulo()).append('\n');
        out.append("ineg_min=").append(negateInt(Integer.MIN_VALUE)).append(',')
                .append(negateInt(-1)).append(',').append(negateInt(7)).append('\n');
        out.append("lneg_min=").append(negateLong(Long.MIN_VALUE)).append(',')
                .append(negateLong(Long.MIN_VALUE + 1)).append('\n');
        out.append("enum_switch=").append(enumSwitch(Season.WINTER)).append(',')
                .append(enumSwitch(Season.SUMMER)).append(',')
                .append(enumSwitch(Season.MONSOON)).append(',')
                .append(enumSwitch(Season.DRY)).append(',')
                .append(enumSwitch(Season.SPRING)).append('\n');
        out.append("array_instanceof=").append(arrayInstanceOf()).append('\n');
        out.append("array_supertypes=").append(arraySupertypes()).append('\n');
        out.append("array_cast=").append(arrayCast()).append('\n');
        return out.toString();
    }
}
