package hfc.cases;

public final class Strings {
    static String concatAll() {
        int i = -3;
        long l = Long.MIN_VALUE;
        float f = 1.5f;
        double d = 2.25;
        char c = 'Z';
        boolean b = true;
        String s = null;
        Object o = 7;
        return i + "|" + l + "|" + f + "|" + d + "|" + c + "|" + b + "|" + s + "|" + o;
    }

    static String switchStrings() {
        StringBuilder out = new StringBuilder();
        for (String s : new String[] {"Aa", "BB", "hello", "", "world"}) {
            switch (s) {
                case "Aa": out.append("Aa"); break;
                case "BB": out.append("BB"); break;
                case "hello": out.append("hi"); break;
                default: out.append("other");
            }
            out.append(',');
        }
        return out.toString();
    }

    static String format() {
        return String.format("%d|%s|%.3f|%x|%08d|%c|%b", 42, "x", 3.14159, 255, 7, 'q', false);
    }

    static String interning() {
        String a = "hello";
        String b = "hel" + "lo";
        String c = new String("hello");
        return (a == b) + "," + (a == c) + "," + (a == c.intern()) + "," + a.equals(c);
    }

    static String splitting() {
        String[] all = "a,b;c".split("[,;]");
        String[] two = "a,b,c".split(",", 2);
        return all.length + ":" + String.join("+", all) + "|" + two.length + ":" + String.join("-", two);
    }

    static String builder() {
        StringBuilder b = new StringBuilder("abc");
        b.insert(1, "XY").delete(2, 4).reverse().append('-').append(12);
        return b.toString();
    }

    static String codePoints() {
        String s = "a\u00e9\u4e16";
        return s.length() + "," + s.codePointCount(0, s.length()) + "," + s.chars().sum();
    }

    static String compares() {
        return "apple".compareTo("banana") + "," + "b".compareTo("a") + ","
                + "abc".compareTo("abc") + "," + "ab".repeat(3) + ","
                + "  x  ".strip() + "|" + "  x  ".stripLeading() + "|";
    }

    static String textBlock() {
        String t = """
            line one
              line two
            """;
        return t.replace("\n", "/");
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("concat=").append(concatAll()).append('\n');
        out.append("switch=").append(switchStrings()).append('\n');
        out.append("format=").append(format()).append('\n');
        out.append("intern=").append(interning()).append('\n');
        out.append("split=").append(splitting()).append('\n');
        out.append("builder=").append(builder()).append('\n');
        out.append("code_points=").append(codePoints()).append('\n');
        out.append("compares=").append(compares()).append('\n');
        out.append("text_block=").append(textBlock()).append('\n');
        return out.toString();
    }
}
