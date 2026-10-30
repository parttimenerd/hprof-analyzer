/**
 * SCENARIO 27 — Many small objects: high instance count, low per-object size
 *
 * THIS IS NOT A BUG. This models a 2D physics simulation or spatial index that
 * legitimately stores millions of small value-type objects. The point is to test
 * hprof-analyzer's histogram and OQL on high-instance-count scenarios.
 *
 * Run:   java -Xmx256m 27_ManySmallObjects.java
 *        (No OOM intended. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f ManySmallObjects) GC.heap_dump heap.hprof
 *
 * What to find:
 *   - Point and Particle appear at the top of the histogram by COUNT
 *   - Each Point is tiny (2 doubles = ~32 bytes with header) but there are millions
 *   - The total size is large even though individual objects are small
 *   - No single object retains a disproportionate amount — retention is flat
 *   - Springs show an interesting 2-reference pattern (connects two Particles)
 *
 * hprof-analyzer:
 *   hprof-analyzer heap histogram heap.hprof
 *   → Point and Particle at top by count; total size is large
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT @displayName, COUNT(*) AS n, SUM(@usedHeapSize) AS totalBytes
 *               FROM INSTANCEOF java.lang.Object
 *               GROUP BY @displayName ORDER BY n DESC LIMIT 10"
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM _27_ManySmallObjects\$Particle"
 *
 *   hprof-analyzer heap report heap.hprof --section overview
 *   → Flat object distribution — no single dominant leaf
 *
 * Note on Project Valhalla: In a future JDK with value types (JEP 401),
 * Point could be declared as a value class and stored as inline data in arrays,
 * eliminating per-object header overhead entirely.
 */
import java.util.*;

public class _27_ManySmallObjects {

    // A 2D point — tiny but useful as a value type
    static final class Point {
        double x, y;
        Point(double x, double y) { this.x = x; this.y = y; }
    }

    // A physics particle: position + velocity + mass + accumulated force
    static final class Particle {
        final Point position;
        final Point velocity;
        double mass;
        double forceX, forceY;

        Particle(double x, double y, double mass) {
            this.position = new Point(x, y);
            this.velocity = new Point(0, 0);
            this.mass     = mass;
        }
    }

    // A spring connecting two particles
    static final class Spring {
        final Particle a, b;
        final double restLength;
        final double stiffness;

        Spring(Particle a, Particle b, double restLength, double stiffness) {
            this.a = a; this.b = b;
            this.restLength = restLength;
            this.stiffness  = stiffness;
        }
    }

    public static void main(String[] args) throws Exception {
        int PARTICLE_COUNT = 1_000_000;
        int SPRING_COUNT   =   500_000;
        Random rng = new Random(42);

        System.out.printf("Creating %,d Particles and %,d Springs …%n",
            PARTICLE_COUNT, SPRING_COUNT);
        System.out.printf("  Each Particle: 2 Points + scalars ≈ ~100 bytes%n");
        System.out.printf("  Each Point: 2 doubles + header ≈ ~24 bytes%n");
        System.out.printf("  Expected Point count: ~%,d (2 per Particle)%n",
            PARTICLE_COUNT * 2);

        Particle[] particles = new Particle[PARTICLE_COUNT];
        for (int i = 0; i < PARTICLE_COUNT; i++) {
            particles[i] = new Particle(
                rng.nextDouble() * 1000,
                rng.nextDouble() * 1000,
                0.1 + rng.nextDouble()
            );
        }

        Spring[] springs = new Spring[SPRING_COUNT];
        for (int i = 0; i < SPRING_COUNT; i++) {
            int a = rng.nextInt(PARTICLE_COUNT);
            int b = rng.nextInt(PARTICLE_COUNT);
            springs[i] = new Spring(particles[a], particles[b],
                10.0 + rng.nextDouble() * 40.0, 1.0 + rng.nextDouble() * 5.0);
        }

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(300);
        System.out.printf("%nHeap used: %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20, rt.maxMemory() >> 20);
        System.out.println();
        System.out.println("Take a heap dump now:");
        System.out.println("  jcmd $(pgrep -f ManySmallObjects) GC.heap_dump heap.hprof");
        System.out.println("Then: hprof-analyzer heap histogram heap.hprof");

        System.out.println(particles[0].position.x + " " + springs[0].restLength);
        Thread.sleep(Long.MAX_VALUE);
    }
}
