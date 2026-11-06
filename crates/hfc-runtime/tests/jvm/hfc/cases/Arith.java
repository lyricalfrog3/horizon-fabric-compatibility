package hfc.cases;

public final class Arith {
    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("int_add_overflow=").append(Integer.MAX_VALUE + 1).append('\n');
        out.append("int_mul_overflow=").append(Integer.MAX_VALUE * 2).append('\n');
        out.append("long_add_overflow=").append(Long.MAX_VALUE + 1L).append('\n');
        out.append("min_div_minus_one=").append(Integer.MIN_VALUE / -1).append('\n');
        out.append("long_min_div_minus_one=").append(Long.MIN_VALUE / -1L).append('\n');
        out.append("mod_negatives=").append(-7 % 3).append(',').append(7 % -3).append(',').append(-7 % -3).append('\n');
        out.append("shifts=").append(1 << 33).append(',').append(-1 >>> 32).append(',').append(-1 >> 1)
                .append(',').append(1L << 65).append(',').append(-1L >>> 64).append('\n');
        out.append("shift_negative=").append(1 << -1).append(',').append(8 >> -1).append('\n');
        out.append("cast_nan=").append((int) Double.NaN).append(',').append((int) Double.POSITIVE_INFINITY)
                .append(',').append((int) -1e20).append(',').append((long) Double.NaN).append('\n');
        out.append("nan_cmp=").append(Double.NaN < 1.0).append(',').append(Double.NaN > 1.0)
                .append(',').append(Double.NaN == Double.NaN).append(',').append(0.0 == -0.0).append('\n');
        out.append("neg_zero=").append(1.0 / -0.0).append(',').append(-0.0 + 0.0).append('\n');
        out.append("floor_mod=").append(Math.floorMod(-7, 3)).append(',').append(Math.floorMod(7, -3))
                .append(',').append(Math.floorMod(Integer.MIN_VALUE, -1)).append('\n');
        out.append("round=").append(Math.round(0.5)).append(',').append(Math.round(-0.5))
                .append(',').append(Math.round(2.5)).append(',').append(Math.round(1.4999999999999998)).append('\n');
        out.append("char_arith=").append((char) ('a' + 1)).append(',').append((int) 'z').append('\n');
        out.append("narrow=").append((byte) 200).append(',').append((short) 70000).append('\n');
        out.append("unsigned_cmp=").append(Long.compareUnsigned(-1L, 1L))
                .append(',').append(Integer.compareUnsigned(-1, 1)).append('\n');
        out.append("binary=").append(Integer.toBinaryString(-1)).append(',')
                .append(Integer.toBinaryString(5)).append(',').append(Long.toHexString(-1L)).append('\n');
        out.append("double_str=").append(0.1 + 0.2).append(',').append(1e21).append(',')
                .append(1e-7).append(',').append(2.0).append('\n');
        return out.toString();
    }
}
