package hfc.cases;

public final class Exceptions {
    static class A extends Exception { A(String m) { super(m); } }
    static class B extends A { B(String m) { super(m); } }

    static String finallyRuns() {
        try { return "try"; } finally { /* no return */ }
    }

    static String finallyOverrides() {
        try { return "try"; } finally { return "finally"; }
    }

    static String breakAndContinue() {
        StringBuilder s = new StringBuilder();
        for (int i = 0; i < 3; i++) {
            try {
                if (i == 1) continue;
                if (i == 2) break;
                s.append('b').append(i);
            } finally {
                s.append('f').append(i);
            }
        }
        return s.toString();
    }

    static String caughtBySuperclass() {
        try {
            throw new B("b");
        } catch (A a) {
            return "caught:" + a.getMessage();
        } finally {
        }
    }

    static String rethrow() {
        try {
            try {
                throw new IllegalArgumentException("x");
            } catch (RuntimeException e) {
                throw new IllegalStateException("wrapped", e);
            }
        } catch (IllegalStateException e) {
            return e.getMessage() + ":" + e.getCause().getMessage();
        }
    }

    static String exceptionInFinally() {
        try {
            try {
                throw new RuntimeException("first");
            } finally {
                throw new RuntimeException("second");
            }
        } catch (RuntimeException e) {
            return e.getMessage();
        }
    }

    static String arithmetic() {
        try {
            return String.valueOf(1 / 0);
        } catch (ArithmeticException e) {
            return e.getClass().getSimpleName() + ":" + e.getMessage();
        }
    }

    static String nulls() {
        String s = null;
        String a;
        try { a = s.length() + ""; } catch (NullPointerException e) { a = "read"; }
        Exceptions n = null;
        String b;
        try { b = n.toString(); } catch (NullPointerException e) { b = "invoke"; }
        return a + "," + b;
    }

    static String bounds() {
        int[] a = new int[2];
        String one;
        try { one = a[2] + ""; } catch (ArrayIndexOutOfBoundsException e) { one = "aioobe"; }
        String two;
        try { int[] b = new int[-1]; two = "" + b.length; } catch (NegativeArraySizeException e) { two = "nase"; }
        return one + "," + two;
    }

    static String casts() {
        Object o = "x";
        try {
            Integer i = (Integer) o;
            return "" + i;
        } catch (ClassCastException e) {
            return "cce";
        }
    }

    static class BadInit { static final int X = 1 / 0; }

    static String staticInit() {
        String a;
        try { new BadInit(); a = "no"; } catch (ExceptionInInitializerError e) { a = "eiie"; }
        String b;
        try { new BadInit(); b = "no"; } catch (NoClassDefFoundError e) { b = "ncdfe"; }
        return a + "," + b;
    }

    static int deep(int n) {
        if (n == 0) throw new IllegalStateException("deep");
        return deep(n - 1);
    }

    static String unwinding() {
        try {
            deep(5);
        } catch (IllegalStateException e) {
            return e.getMessage();
        }
        return "none";
    }

    static String resources() {
        final StringBuilder order = new StringBuilder();
        class R implements AutoCloseable {
            final String n;
            R(String n) { this.n = n; }
            public void close() { order.append("close").append(n); }
        }
        try (R a = new R("a"); R b = new R("b")) {
            order.append("body");
            throw new RuntimeException("boom");
        } catch (RuntimeException e) {
            order.append(';').append(e.getMessage()).append(';').append(e.getSuppressed().length);
            for (Throwable t : e.getSuppressed()) order.append(',').append(t.getMessage());
        }
        return order.toString();
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("finally_runs=").append(finallyRuns()).append('\n');
        out.append("finally_overrides=").append(finallyOverrides()).append('\n');
        out.append("break_continue=").append(breakAndContinue()).append('\n');
        out.append("superclass=").append(caughtBySuperclass()).append('\n');
        out.append("rethrow=").append(rethrow()).append('\n');
        out.append("exception_in_finally=").append(exceptionInFinally()).append('\n');
        out.append("arithmetic=").append(arithmetic()).append('\n');
        out.append("nulls=").append(nulls()).append('\n');
        out.append("bounds=").append(bounds()).append('\n');
        out.append("casts=").append(casts()).append('\n');
        out.append("static_init=").append(staticInit()).append('\n');
        out.append("unwinding=").append(unwinding()).append('\n');
        out.append("resources=").append(resources()).append('\n');
        return out.toString();
    }
}
