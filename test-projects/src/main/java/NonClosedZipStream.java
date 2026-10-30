/**
 * Leaks memory accourding to <a href="https://stackoverflow.com/questions/37586096/why-does-my-bufferedreader-code-leak-memory">stackoverflow</a>
 */

import java.io.*;
import java.nio.file.*;
import java.util.*;
import java.util.zip.*;

public class NonClosedZipStream {

    // Intentionally retain readers in memory.
    private static final List<BufferedReader> leakedReaders =
            new ArrayList<>();

    public static void main(String[] args) throws Exception {

        Path file = Files.createTempFile("memory-leak-", ".gz");
        file.toFile().deleteOnExit();

        // Create a small compressed test file.
        try (GZIPOutputStream gzip = new GZIPOutputStream(
                Files.newOutputStream(file));
             BufferedWriter writer = new BufferedWriter(
                     new OutputStreamWriter(gzip))) {

            for (int i = 0; i < 1000; i++) {
                writer.write("Sample CSV data," + i);
                writer.newLine();
            }
        }
        for (int i = 1; i <= 1000; i++) {

            // Open a new reader each iteration.
            BufferedReader reader = new BufferedReader(
                    new InputStreamReader(
                            new GZIPInputStream(
                                    Files.newInputStream(file))),
                    131072
            );

            // Read the file completely.
            while (reader.readLine() != null) {
                // Process line
            }

            // Intentionally retain the reader without closing it — this holds the
            // GZIPInputStream (and its native Inflater) in memory.
            leakedReaders.add(reader);
        }
        Thread.sleep(Integer.MAX_VALUE);
    }
}