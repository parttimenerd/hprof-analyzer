/**
 * LEAK 36 — ClassLoader leak via DEPLOYMENT_REGISTRY (webapp redeploy pattern)
 *
 * Simulates a servlet-container / OSGi / plugin-host that performs 10 "hot
 * redeploys" but forgets to remove each deployment's context from a global
 * registry on undeploy. The stale ClassLoaders stay alive on the heap because
 * DEPLOYMENT_REGISTRY holds them strongly — and because live instances of the
 * deployed classes hold a back-reference to their defining ClassLoader, the
 * entire deployment (loader + all domain objects) is pinned in memory.
 *
 * Run:   java -Xmx256m 36_ClassLoaderLeak.java
 *
 * Capture (after "HEAP_DUMP_READY" is printed):
 *   jcmd $(pgrep -f ClassLoaderLeak) GC.heap_dump heap.hprof
 *
 * What's in the heap:
 *   10 URLClassLoader instances                → one per simulated deployment
 *   10 × 100 = 1000 DomainService proxy objs  → created inside each loader
 *   10 DeploymentContext wrappers              → holds loader + object list
 *   DEPLOYMENT_REGISTRY (ArrayList)            → the leak root
 *
 * hprof-analyzer should report:
 *   - Headline Retainer: DEPLOYMENT_REGISTRY / DeploymentContext (or Proxy internals)
 *   - 10 ClassLoader suspects in the dominator tree
 *
 * Interesting analysis:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → root path leads to _36_ClassLoaderLeak.DEPLOYMENT_REGISTRY static field
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM java.net.URLClassLoader"
 *   → should return 10
 *
 *   hprof-analyzer heap report heap.hprof --section triage
 *   → headline-retainer fires; concentration signal if registry dominates
 *
 * Real-world form (Tomcat / Spring / OSGi):
 *   - Static Map<String, WebappClassLoader> in a framework singleton
 *   - JDBC driver registered but never deregistered (DriverManager holds loader)
 *   - Shutdown hook or MBean that captured a webapp-class reference
 *   - ThreadLocal holding a webapp-class instance on a container thread
 *
 * Fix:
 *   On undeploy, call DEPLOYMENT_REGISTRY.remove(ctx) — or, better, use a
 *   WeakReference<ClassLoader> wrapper so the registry entry is automatically
 *   cleared when the loader becomes unreachable. Also: deregister JDBC drivers,
 *   cancel timers, and interrupt any threads started by the webapp.
 *   Library: mjiderhamn/classloader-leak-prevention (Maven Central)
 */
import java.lang.reflect.*;
import java.net.*;
import java.util.*;

public class _36_ClassLoaderLeak {

    // ── Deployment model ──────────────────────────────────────────────────────

    /** Represents one webapp deployment: its classloader + live domain objects. */
    static class DeploymentContext {
        final int            deploymentId;
        final ClassLoader    loader;      // ← strong ref keeps loader alive
        final List<Object>   services;    // ← domain objects defined in loader

        DeploymentContext(int id, ClassLoader loader, List<Object> services) {
            this.deploymentId = id;
            this.loader       = loader;
            this.services     = services;
        }
    }

    /** ❌ BAD: never cleared on undeploy — keeps every old deployment alive. */
    static final List<DeploymentContext> DEPLOYMENT_REGISTRY = new ArrayList<>();

    // ── Deployment simulation ─────────────────────────────────────────────────

    static final int DEPLOYMENTS     = 10;
    static final int SERVICES_PER_DEPLOY = 100;

    /**
     * Simulate a single webapp deployment:
     *   1. Create a new URLClassLoader (stands in for a WebappClassLoader)
     *   2. Create SERVICES_PER_DEPLOY Proxy objects inside that loader
     *      (stands in for domain beans / controllers loaded from the webapp JAR)
     *   3. Register the context globally — never removed (the leak)
     */
    static void deploy(int id) throws Exception {
        URLClassLoader cl = new URLClassLoader(
            new URL[0],
            _36_ClassLoaderLeak.class.getClassLoader()
        );

        List<Object> services = new ArrayList<>(SERVICES_PER_DEPLOY);
        for (int i = 0; i < SERVICES_PER_DEPLOY; i++) {
            // Proxy defined inside `cl` → its Class object references `cl`
            Object svc = Proxy.newProxyInstance(
                cl,
                new Class[]{ Runnable.class, AutoCloseable.class },
                (proxy, method, args) -> null
            );
            services.add(svc);
        }

        DeploymentContext ctx = new DeploymentContext(id, cl, services);
        DEPLOYMENT_REGISTRY.add(ctx);  // ← leak: old deployment never removed

        System.out.printf("  deploy #%2d: loader=%s  services=%d%n",
            id, cl.getClass().getSimpleName(), services.size());
    }

    // ── Main ──────────────────────────────────────────────────────────────────

    public static void main(String[] args) throws Exception {
        System.out.println("=== ClassLoader Leak — Webapp Redeploy Pattern ===");
        System.out.printf("Simulating %d hot redeploys (%d services each) …%n",
            DEPLOYMENTS, SERVICES_PER_DEPLOY);

        for (int i = 1; i <= DEPLOYMENTS; i++) {
            deploy(i);
            // Simulate brief app activity between redeploys
            Thread.sleep(10);
        }

        System.gc();
        Thread.sleep(200);

        Runtime rt = Runtime.getRuntime();
        long used = rt.totalMemory() - rt.freeMemory();
        System.out.printf("%nDone | heap used %,d MB / max %,d MB%n",
            used >> 20, rt.maxMemory() >> 20);
        System.out.printf("  DEPLOYMENT_REGISTRY: %d retained deployments%n",
            DEPLOYMENT_REGISTRY.size());
        System.out.printf("  Total leaked services: %,d%n",
            DEPLOYMENT_REGISTRY.stream().mapToInt(c -> c.services.size()).sum());
        System.out.println();
        System.out.println("HEAP_DUMP_READY");
        System.out.println("  jcmd $(pgrep -f ClassLoaderLeak) GC.heap_dump heap.hprof");
        Thread.sleep(Long.MAX_VALUE);
    }
}
