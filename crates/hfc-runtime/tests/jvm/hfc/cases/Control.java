package hfc.cases;

public final class Control {
    static String dense(int x) {
        switch (x) {
            case 0: return "z";
            case 1: return "o";
            case 2: return "t";
            case 3: return "th";
            default: return "d";
        }
    }

    static String sparse(int x) {
        switch (x) {
            case -100: return "a";
            case 7: return "b";
            case 1000: return "c";
            default: return "d";
        }
    }

    static String expression(int x) {
        int y = switch (x) {
            case 1, 2 -> 10;
            case 3 -> { int z = x * 2; yield z + 1; }
            default -> -1;
        };
        return "" + y;
    }

    static String labelled() {
        StringBuilder s = new StringBuilder();
        outer:
        for (int i = 0; i < 3; i++) {
            for (int j = 0; j < 3; j++) {
                if (j == 2) continue outer;
                if (i == 2) break outer;
                s.append(i).append(j);
            }
        }
        return s.toString();
    }

    static String longLoop() {
        long sum = 0;
        for (int i = 0; i < 100000; i++) sum += i;
        return "" + sum;
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("dense=").append(dense(0)).append(dense(1)).append(dense(2))
                .append(dense(3)).append(dense(9)).append('\n');
        out.append("sparse=").append(sparse(-100)).append(sparse(7)).append(sparse(1000))
                .append(sparse(0)).append('\n');
        out.append("expression=").append(expression(1)).append(expression(3)).append(expression(9)).append('\n');
        out.append("labelled=").append(labelled()).append('\n');
        out.append("long_loop=").append(longLoop()).append('\n');
        return out.toString();
    }
}
