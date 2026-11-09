package hfc.cases;

import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.function.Function;
import java.util.function.Supplier;
import java.util.stream.Collectors;
import java.util.stream.IntStream;

public final class Lambdas {
    static int twice(int n) { return n * 2; }

    static String functions() {
        Function<Integer, Integer> f = x -> x * 2;
        Supplier<String> s = () -> "sup";
        Runnable r = () -> { };
        r.run();
        return f.apply(21) + "," + s.get();
    }

    static String references() {
        Function<Integer, Integer> stat = Lambdas::twice;
        List<String> list = new ArrayList<>(List.of("b", "a", "c"));
        Function<String, Integer> bound = list::indexOf;
        Function<String, Integer> unbound = String::length;
        Supplier<ArrayList<String>> ctor = ArrayList::new;
        return stat.apply(4) + "," + bound.apply("c") + "," + unbound.apply("abcd")
                + "," + ctor.get().size();
    }

    static String captureLoop() {
        List<Supplier<Integer>> all = new ArrayList<>();
        for (int i = 0; i < 3; i++) {
            final int value = i;
            all.add(() -> value);
        }
        StringBuilder out = new StringBuilder();
        for (Supplier<Integer> s : all) out.append(s.get());
        return out.toString();
    }

    interface Maker<T> { T make(String s); }

    static String bridge() {
        Maker<String> m = String::new;
        return m.make("xy");
    }

    static String comparing() {
        List<String> words = new ArrayList<>(List.of("bbb", "a", "cc", "dd"));
        words.sort(Comparator.comparing(String::length).thenComparing(Comparator.naturalOrder()));
        return String.join(",", words);
    }

    static String streams() {
        List<Integer> nums = IntStream.rangeClosed(1, 6).boxed().collect(Collectors.toList());
        String mapped = nums.stream()
                .filter(n -> n % 2 == 0)
                .map(n -> n * 10)
                .sorted()
                .map(String::valueOf)
                .collect(Collectors.joining("+"));
        List<String> names = List.of("alice", "bob", "anna");
        Map<Integer, List<String>> byLength = names.stream()
                .collect(Collectors.groupingBy(String::length, TreeMap::new, Collectors.toList()));
        return mapped + "|" + byLength;
    }

    static String extremes() {
        List<String> words = List.of("pear", "fig", "banana", "kiwi");
        String longest = words.stream().max(Comparator.comparingInt(String::length)).orElse("none");
        String shortest = words.stream().min(Comparator.comparingInt(String::length)).orElse("none");
        String tieFirst = List.of("ab", "cd").stream().max(Comparator.comparingInt(String::length)).get();
        String empty = List.<String>of().stream().max(Comparator.naturalOrder()).orElse("none");
        return longest + "," + shortest + "," + tieFirst + "," + empty;
    }

    static String treeSet() {
        java.util.TreeSet<Integer> set = new java.util.TreeSet<>(Comparator.reverseOrder());
        set.addAll(List.of(3, 1, 2));
        return set + ":" + set.first();
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("functions=").append(functions()).append('\n');
        out.append("references=").append(references()).append('\n');
        out.append("capture_loop=").append(captureLoop()).append('\n');
        out.append("bridge=").append(bridge()).append('\n');
        out.append("comparing=").append(comparing()).append('\n');
        out.append("streams=").append(streams()).append('\n');
        out.append("extremes=").append(extremes()).append('\n');
        out.append("tree_set=").append(treeSet()).append('\n');
        return out.toString();
    }
}
