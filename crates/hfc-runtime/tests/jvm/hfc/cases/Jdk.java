package hfc.cases;

import java.io.File;
import java.io.IOException;
import java.nio.file.Files;

/**
 * The JDK host boundary: the {@code java.io.File}/{@code UnixFileSystem}
 * natives, the system properties they read, identity hashes, string interning
 * and the float bit conversions. Everything is printed as a comparison so the
 * JVM's absolute paths never appear in the output.
 */
public final class Jdk {
    /** Booleans as text: a concat recipe in the interpreter prints 1/0. */
    static final class Caller {
        static Class<?> whoCalled() {
            return StackWalker.getInstance(StackWalker.Option.RETAIN_CLASS_REFERENCE).getCallerClass();
        }
    }

    private static String b(boolean value) {
        return value ? "true" : "false";
    }

    public static String run() throws IOException {
        StringBuilder out = new StringBuilder();

        // --- System.identityHashCode: stable per object, distinct between them.
        Object a = new Object();
        Object b = new Object();
        out.append("identity_stable=").append(b(System.identityHashCode(a) == System.identityHashCode(a))).append('\n');
        out.append("identity_distinct=").append(b(System.identityHashCode(a) != System.identityHashCode(b))).append('\n');
        out.append("identity_null=").append(System.identityHashCode(null)).append('\n');

        // --- String.intern: the canonical instance for the text.
        String built = new StringBuilder("hfc").append("-intern").toString();
        out.append("intern=").append(b(built.intern() == "hfc-intern")).append(',')
                .append(b("hfc-intern" == "hfc-intern")).append('\n');

        // --- Float.floatToRawIntBits / intBitsToFloat.
        out.append("float_bits=").append(Float.floatToRawIntBits(1.0f)).append(',')
                .append(Float.floatToRawIntBits(-0.0f)).append(',')
                .append(b(Float.intBitsToFloat(0x3f800000) == 1.0f)).append('\n');
        out.append("float_nan_bits=").append(Float.floatToIntBits(Float.NaN)).append('\n');

        // --- Runtime.availableProcessors: a real host answer, never zero.
        out.append("processors=").append(b(Runtime.getRuntime().availableProcessors() > 0)).append('\n');

        // --- System.nanoTime is monotonic.
        long first = System.nanoTime();
        long second = System.nanoTime();
        out.append("nano_monotonic=").append(b(second >= first)).append('\n');

        // --- The system properties the JDK classes themselves read.
        out.append("prop_userdir=").append(b(System.getProperty("user.dir") != null)).append(',')
                .append(System.getProperty("file.separator")).append(',')
                .append(System.getProperty("path.separator")).append('\n');
        System.setProperty("hfc.test", "v1");
        out.append("prop_set=").append(System.getProperty("hfc.test")).append(',')
                .append(System.getProperty("hfc.missing", "d")).append('\n');
        out.append("prop_view=").append(System.getProperties().getProperty("hfc.test")).append('\n');

        // --- java.io.File over UnixFileSystem: create, list, canonicalize, move, delete.
        File base = new File("jdk-case");
        new File(new File(base, "sub"), "notes.txt").delete();
        new File(new File(base, "sub"), "moved.txt").delete();
        new File(base, "sub").delete();
        base.delete();
        File dir = new File(base, "sub");
        out.append("mkdirs=").append(b(dir.mkdirs())).append(',').append(b(dir.isDirectory())).append('\n');
        File file = new File(dir, "notes.txt");
        out.append("create=").append(b(file.createNewFile())).append(',')
                .append(b(file.createNewFile())).append(',').append(b(file.exists())).append('\n');
        out.append("kind=").append(b(file.isFile())).append(',').append(b(file.isDirectory())).append('\n');
        out.append("names=").append(file.getParent()).append(',').append(file.getName()).append('\n');
        out.append("absolute=").append(b(file.isAbsolute())).append(',')
                .append(b(file.getAbsoluteFile().isAbsolute())).append('\n');
        File spelled = new File("jdk-case/./sub/../sub/notes.txt");
        out.append("canonical_same=").append(b(spelled.getCanonicalPath().equals(file.getCanonicalPath()))).append('\n');
        out.append("canonical_suffix=")
                .append(b(file.getCanonicalPath().endsWith("jdk-case" + File.separator + "sub" + File.separator + "notes.txt")))
                .append('\n');
        Files.writeString(file.toPath(), "hello");
        out.append("length=").append(file.length()).append(',').append(b(file.lastModified() > 0L)).append('\n');
        String[] listed = dir.list();
        out.append("list=").append(listed == null ? "null" : String.join(",", listed)).append('\n');
        File moved = new File(dir, "moved.txt");
        out.append("rename=").append(b(file.renameTo(moved))).append(',').append(b(moved.exists())).append(',')
                .append(b(file.exists())).append('\n');
        out.append("delete=").append(b(moved.delete())).append(',').append(b(moved.exists())).append('\n');
        out.append("rmdir=").append(b(dir.delete())).append(',').append(b(base.delete())).append('\n');
        out.append("tofile=").append(new File("jdk-case/sub").toPath().toFile().getPath()).append('\n');

        // --- StackWalker: the interpreter has no frames, and a JVM may report
        // none; what must work is that `walk` runs the function it is given.
        String walked = StackWalker.getInstance().walk(stream -> "answered");
        out.append("stackwalker=").append(walked).append('\n');
        // The process environment is the host's; both processes inherit PATH.
        String path = System.getenv("PATH");
        out.append("env=").append(b(path != null && !path.isEmpty())).append(',')
                .append(b(System.getenv("HFC_SURELY_UNSET_VARIABLE") == null)).append(',')
                .append(b(System.getenv().get("PATH") != null)).append('\n');
        out.append("caller=").append(Caller.whoCalled().getName()).append('\n');

        return out.toString();
    }
}
