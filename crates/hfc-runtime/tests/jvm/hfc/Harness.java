package hfc;

/** Runs each named case's {@code run()} on the real JVM; the differential
 * test runs the same classes in the interpreter. */
public final class Harness {
    public static void main(String[] args) {
        for (String name : args) {
            System.out.println("=== " + name);
            try {
                Class<?> type = Class.forName(name);
                Object out = type.getMethod("run").invoke(null);
                System.out.print(out);
            } catch (java.lang.reflect.InvocationTargetException e) {
                Throwable cause = e.getCause();
                System.out.println("UNCAUGHT " + cause.getClass().getName() + ": " + cause.getMessage());
            } catch (Throwable t) {
                System.out.println("UNCAUGHT " + t.getClass().getName() + ": " + t.getMessage());
            }
        }
    }
}
