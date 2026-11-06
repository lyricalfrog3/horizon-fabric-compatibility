package hfc.cases;

public final class Objects {
    interface Greeter {
        default String greet() { return "Greeter"; }
        static String kind() { return "iface"; }
    }

    interface Left extends Greeter {
        default String greet() { return "Left+" + Greeter.super.greet(); }
    }

    interface Right extends Greeter {
        default String greet() { return "Right+" + Greeter.super.greet(); }
    }

    abstract static class Base implements Greeter {
        protected String name = "base";
        String field() { return name; }
        abstract String who();
        String describe() { return who() + "/" + field(); }
    }

    static class Both extends Base implements Left, Right {
        protected String name = "both";
        Both() { super(); }
        @Override public String greet() { return "Both+" + Left.super.greet() + "+" + Right.super.greet(); }
        String who() { return "both"; }
        String superField() { return super.field() + "/" + ((Base) this).name; }
    }

    static class InitA { static final int A = InitB.B + 1; }
    static class InitB { static final int B = InitA.A + 1; }

    record Point(int x, int y) {
        Point { if (x < 0 || y < 0) throw new IllegalArgumentException("neg"); }
        int sum() { return x + y; }
    }

    enum Colour {
        RED { int code() { return 1; } },
        GREEN { int code() { return 2; } },
        BLUE { int code() { return 3; } };
        abstract int code();
    }

    static String capture() {
        final int local = 7;
        class Inner {
            int plus(int n) { return local + n; }
        }
        Runnable r = new Runnable() {
            public void run() { }
        };
        r.run();
        return new Inner().plus(3) + "";
    }

    static String arrays() {
        int[][] grid = new int[2][3];
        grid[1][2] = 5;
        int[] one = {1, 2, 3};
        int[] copy = one.clone();
        copy[0] = 9;
        return grid.length + "," + grid[1].length + "," + grid[1][2] + ","
                + one[0] + "," + copy[0] + "," + (one == copy);
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        Both both = new Both();
        Base asBase = both;
        out.append("dispatch=").append(asBase.describe()).append(',').append(both.greet()).append('\n');
        out.append("super_field=").append(both.superField()).append('\n');
        out.append("interface_static=").append(Greeter.kind()).append('\n');
        out.append("init_order=");
        try { out.append(InitA.A).append(',').append(InitB.B); }
        catch (Throwable t) { out.append(t.getClass().getSimpleName()); }
        out.append('\n');
        Object o = both;
        out.append("instanceof=").append(o instanceof Both).append(',').append(o instanceof Greeter)
                .append(',').append(o instanceof Point).append('\n');
        Point p = new Point(2, 3);
        Point q = new Point(2, 3);
        out.append("record=").append(p.equals(q)).append(',').append(p.hashCode() == q.hashCode())
                .append(',').append(p).append(',').append(p.sum()).append('\n');
        String neg;
        try { new Point(-1, 0); neg = "no"; } catch (IllegalArgumentException e) { neg = e.getMessage(); }
        out.append("record_check=").append(neg).append('\n');
        out.append("enum=").append(Colour.values().length).append(',').append(Colour.valueOf("GREEN").ordinal())
                .append(',').append(Colour.BLUE.code()).append(',');
        switch (Colour.valueOf("RED")) {
            case RED: out.append("red"); break;
            case GREEN: out.append("green"); break;
            default: out.append("other");
        }
        out.append('\n');
        out.append("capture=").append(capture()).append('\n');
        out.append("arrays=").append(arrays()).append('\n');
        return out.toString();
    }
}
