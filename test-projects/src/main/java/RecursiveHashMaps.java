import me.bechberger.femtocli.FemtoCli;
import me.bechberger.femtocli.annotations.Parameters;

import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;

/** Creates a tree data structure with 10 levels of depth 4 children each,
 * with the lowest level having only one child (the root) if passed "complex"/"normal" (default) + depth as an argument
 * Every node has a HashMap with 100 String to String entries of a fixed size. It then waits forever.
 * <p>
 * This should crash because it needs more than 5GB of memory, use smaller depth values like 5 to not crash */
public class RecursiveHashMaps implements Runnable {
    static class Node {
        HashMap<String, String> map;
        List<Node> children;

        Node(int maxDepth, boolean complex) {
            map = new HashMap<>();
            for (int i = 0; i < 100; i++) {
                map.put("key" + i, "value" + i);
            }
            if (maxDepth > 0) {
                children = new ArrayList<>();
                int numChildren = complex && maxDepth == 1 ? 1 : 4;
                for (int i = 0; i < numChildren; i++) {
                    children.add(new Node(maxDepth - 1, complex));
                }
            }
        }
    }

    @Parameters(defaultValue = "normal", index = "0")
    String mode = "normal";

    @Parameters(defaultValue = "10", index = "1")
    int depth = 10;


    @Override
    public void run() {
        new Node(depth, mode.equals("complex"));
        try {
            Thread.sleep(Integer.MAX_VALUE);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    static void main(String[] args) {
        FemtoCli.run(new RecursiveHashMaps(), args);
    }
}
