package hfc.cases;

import java.io.File;

public final class Probe {
    public static String run() {
        File f = new File("config/jamlib/known.txt");
        File p = f.getParentFile();
        return "path=" + f.getPath() + "\nparent=" + (p == null ? "null" : p.getPath())
                + "\nabs=" + f.getAbsolutePath() + "\n";
    }
}
