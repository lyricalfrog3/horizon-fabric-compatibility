package hfc.cases;

import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.ConcurrentModificationException;
import java.util.HashMap;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.LinkedList;
import java.util.List;
import java.util.TreeMap;

public final class Collections {
    static String lists() {
        List<String> a = new ArrayList<>();
        a.add("x");
        a.add("y");
        a.add(1, "m");
        a.remove("x");
        return a + ":" + a.size() + ":" + a.get(1);
    }

    static String linked() {
        LinkedList<Integer> l = new LinkedList<>();
        l.add(1);
        l.addFirst(0);
        l.addLast(2);
        l.removeFirst();
        return l.toString();
    }

    static String deque() {
        ArrayDeque<String> d = new ArrayDeque<>();
        d.push("a");
        d.push("b");
        d.addLast("c");
        return d.pollFirst() + "," + d.pollLast() + "," + d.size();
    }

    static String tree() {
        TreeMap<String, Integer> m = new TreeMap<>();
        m.put("b", 2);
        m.put("a", 1);
        m.put("c", 3);
        return m + ":" + m.firstKey() + ":" + m.lastKey();
    }

    static String linkedMap() {
        LinkedHashMap<String, Integer> m = new LinkedHashMap<>();
        m.put("z", 1);
        m.put("a", 2);
        m.put("m", 3);
        return m.toString();
    }

    static String hashStrings() {
        HashMap<String, Integer> m = new HashMap<>();
        for (String k : List.of("a", "b", "c", "d", "e")) m.put(k, k.length());
        return m.toString();
    }

    static String hashIntegers() {
        HashMap<Integer, String> m = new HashMap<>();
        for (int i = 1; i <= 5; i++) m.put(i, "v" + i);
        return m.toString();
    }

    static String stable() {
        List<String> w = new ArrayList<>(List.of("bb", "aa", "c", "dd", "e"));
        w.sort((x, y) -> Integer.compare(x.length(), y.length()));
        return String.join(",", w);
    }

    static String immutable() {
        List<String> l = List.of("a");
        try {
            l.add("b");
            return "no";
        } catch (UnsupportedOperationException e) {
            return "uoe";
        }
    }

    static String iteratorRemove() {
        List<Integer> l = new ArrayList<>(List.of(1, 2, 3, 4));
        Iterator<Integer> it = l.iterator();
        while (it.hasNext()) {
            if (it.next() % 2 == 0) it.remove();
        }
        return l.toString();
    }

    static String concurrentModification() {
        List<Integer> l = new ArrayList<>(List.of(1, 2, 3));
        try {
            for (Integer i : l) {
                if (i == 2) l.remove(i);
            }
            return "no";
        } catch (ConcurrentModificationException e) {
            return "cme";
        }
    }

    public static String run() {
        StringBuilder out = new StringBuilder();
        out.append("lists=").append(lists()).append('\n');
        out.append("linked=").append(linked()).append('\n');
        out.append("deque=").append(deque()).append('\n');
        out.append("tree=").append(tree()).append('\n');
        out.append("linked_map=").append(linkedMap()).append('\n');
        out.append("hash_strings=").append(hashStrings()).append('\n');
        out.append("hash_integers=").append(hashIntegers()).append('\n');
        out.append("stable=").append(stable()).append('\n');
        out.append("immutable=").append(immutable()).append('\n');
        out.append("iterator_remove=").append(iteratorRemove()).append('\n');
        out.append("cme=").append(concurrentModification()).append('\n');
        return out.toString();
    }
}
