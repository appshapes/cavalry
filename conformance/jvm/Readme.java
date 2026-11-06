// Runs the example of clients/jvm/README.md against the runner's driver; run.sh calls it
// after Quickstart, so the README's program is a test (CLAUDE.md, "Clients and developer
// experience"; .claude/rules/docs.md, "Code in docs"). The README's program is
// clients/jvm/example/Example.java: this checks that the README's Java block is that
// file, byte for byte, runs it with this Java in source-file mode against the classes
// run.sh compiled, as the README's `java ... clients/jvm/example/Example.java` line does,
// for up to 10 s, and exits 1 unless it exits 0 and prints the line the README promises
// after "You will see". The README's shell lines around the program, the builds and the
// driver, are run.sh's own steps. Run from the checkout's root. Java's standard library
// only (ADR 0003).

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.concurrent.TimeUnit;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

public final class Readme {
    static final String README = "clients/jvm/README.md";
    static final String EXAMPLE = "clients/jvm/example/Example.java";
    static final long WAIT_SECONDS = 10;

    static void fail(String what) {
        System.err.println("conformance/jvm: " + README + "'s example: " + what);
        System.exit(1);
    }

    public static void main(String[] args) throws IOException, InterruptedException {
        String text = Files.readString(Path.of(README), StandardCharsets.UTF_8);
        Matcher program = Pattern.compile("(?s)```java\n(.*?)```\n").matcher(text);
        Matcher expected = Pattern.compile("(?m)^You will see `([^`]+)`\\.$").matcher(text);
        if (!program.find() || !expected.find()) {
            fail("no ```java block, or no line \"You will see `...`.\" after it; keep both shapes so this test can find them");
        }
        String example = Files.readString(Path.of(EXAMPLE), StandardCharsets.UTF_8);
        if (!program.group(1).equals(example)) {
            fail("the README's Java block is not " + EXAMPLE + "; the README shows the program that runs, so change both");
        }
        String java = Path.of(System.getProperty("java.home"), "bin", "java").toString();
        Process run = new ProcessBuilder(List.of(java, "--enable-native-access=ALL-UNNAMED", "-cp",
            "clients/jvm/build/classes", EXAMPLE)).redirectErrorStream(false).start();
        // Both streams are read after the wait: the program prints one line and the
        // runtime's warnings, if any, fit the pipe.
        if (!run.waitFor(WAIT_SECONDS, TimeUnit.SECONDS)) {
            run.destroyForcibly();
            fail("did not finish in " + WAIT_SECONDS + " s; it printed " + quote(new String(run.getInputStream().readAllBytes(), StandardCharsets.UTF_8)));
        }
        String stdout = new String(run.getInputStream().readAllBytes(), StandardCharsets.UTF_8);
        String stderr = new String(run.getErrorStream().readAllBytes(), StandardCharsets.UTF_8);
        if (run.exitValue() != 0) {
            fail("exited with " + run.exitValue() + "; it printed " + quote(stdout) + " and " + quote(stderr));
        }
        String line = expected.group(1);
        for (String printed : stdout.split("\n")) {
            if (printed.equals(line)) {
                System.out.println("conformance/jvm: " + README + "'s example printed `" + line + "`, as the README says");
                return;
            }
        }
        fail("printed " + quote(stdout) + ", not the line the README says the reader will see, " + quote(line));
    }

    static String quote(String s) {
        return "\"" + s.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n") + "\"";
    }
}
