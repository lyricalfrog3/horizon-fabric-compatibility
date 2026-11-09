package hfc.cases;

import java.util.concurrent.TimeUnit;

/** `long`/`double` values through the dup2/pop2 family of stack operations. */
public final class Wide {
    static long counter;
    static double total;
    long field;
    double ratio;

    static long tick() {
        return ++counter;
    }

    static String dup2() {
        long a = counter = 40L;
        double b = total = 2.5;
        return a + "," + b;
    }

    static String dup2X1() {
        Wide w = new Wide();
        long x = w.field = 7L;
        double y = w.ratio = 0.25;
        w.field += 3;
        return x + "," + y + "," + w.field;
    }

    static String dup2X2() {
        long[] longs = new long[2];
        double[] doubles = new double[1];
        long x = longs[1] = 9L;
        double y = doubles[0] = 1.5;
        longs[0] += 4;
        long post = longs[1]++;
        return x + "," + y + "," + longs[0] + "," + post + "," + longs[1];
    }

    static String pop2() {
        counter = 0;
        tick();
        tick();
        Math.max(1.0, 2.0);
        return String.valueOf(counter);
    }

    static String timeUnit() {
        return TimeUnit.MILLISECONDS.toNanos(10) + "," + TimeUnit.SECONDS.toMillis(2) + ","
                + TimeUnit.NANOSECONDS.convert(3, TimeUnit.MICROSECONDS);
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("dup2=").append(dup2()).append('\n');
        out.append("dup2_x1=").append(dup2X1()).append('\n');
        out.append("dup2_x2=").append(dup2X2()).append('\n');
        out.append("pop2=").append(pop2()).append('\n');
        out.append("time_unit=").append(timeUnit()).append('\n');
        return out.toString();
    }
}
