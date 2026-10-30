/**
 * SCENARIO 25 — Deep object chain: long linked list and deep tree
 *
 * THIS IS NOT A BUG. Tests how hprof-analyzer handles deep dominator chains
 * where retained size is distributed along a long path rather than concentrated
 * at a single node.
 *
 * Run:   java -Xmx128m -XX:+HeapDumpOnOutOfMemoryError -XX:HeapDumpPath=heap.hprof \
 *             25_DeepObjectChain.java
 *        (No OOM intended. Heap dump mid-run.)
 *
 * Capture:  jcmd $(pgrep -f DeepObjectChain) GC.heap_dump heap.hprof
 *
 * What to find:
 *   - LINKED_LIST_HEAD is the GC root; its retained size = 100% of the chain
 *   - Each node dominates the rest of the chain below it
 *   - The dominator tree is linear (depth = N) rather than wide (fan-out)
 *   - TREE_ROOT retains a binary tree; each subtree root dominates its children
 *   - Histogram shows many Node / TreeNode instances of identical shallow size
 *
 * Interesting analysis angle:
 *   The dominator tree for a linked list is just the list itself — the retained
 *   size of node[0] equals the total list size. The dominator tree for a DAG
 *   (where two paths join) is different — the join point is the dominator.
 *
 * hprof-analyzer:
 *   hprof-analyzer heap report heap.hprof --section leaks
 *   → LINKED_LIST_HEAD or TREE_ROOT as top retainer (no bug, just large chain)
 *
 *   hprof-analyzer heap browse heap.hprof
 *   → Navigate the dominator tree interactively; see chain depth
 *
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM _25_DeepObjectChain\$Node"
 *   hprof-analyzer heap query heap.hprof \
 *       --oql "SELECT COUNT(*) FROM _25_DeepObjectChain\$TreeNode"
 */
public class _25_DeepObjectChain {

    // ---- Linked list node -------------------------------------------------------
    static final class Node {
        final int     id;
        final byte[]  payload = new byte[512]; // 512 B per node
        Node          next;

        Node(int id) { this.id = id; }
    }

    // ---- Binary tree node -------------------------------------------------------
    static final class TreeNode {
        final int      value;
        final byte[]   data = new byte[256]; // 256 B per tree node
        TreeNode       left, right;

        TreeNode(int value) { this.value = value; }
    }

    static Node     LINKED_LIST_HEAD;
    static TreeNode TREE_ROOT;

    static Node buildLinkedList(int length) {
        Node head = new Node(0);
        Node cur  = head;
        for (int i = 1; i < length; i++) {
            cur.next = new Node(i);
            cur = cur.next;
        }
        return head;
    }

    // Build a balanced BST from sorted range [lo, hi)
    static TreeNode buildTree(int lo, int hi) {
        if (lo >= hi) return null;
        int mid = (lo + hi) / 2;
        TreeNode node = new TreeNode(mid);
        node.left  = buildTree(lo, mid);
        node.right = buildTree(mid + 1, hi);
        return node;
    }

    public static void main(String[] args) throws Exception {
        int LIST_LEN  = 50_000;  // 50 000 nodes × 512 B = ~25 MB
        int TREE_SIZE = 65_535;  // 2^16 - 1 nodes (full binary tree, depth 16) × 256 B ≈ 16 MB

        System.out.printf("Building linked list: %,d nodes × 512 B = ~%,d MB%n",
            LIST_LEN, LIST_LEN * 512 / 1024 / 1024);
        LINKED_LIST_HEAD = buildLinkedList(LIST_LEN);

        System.out.printf("Building binary tree: %,d nodes × 256 B = ~%,d MB%n",
            TREE_SIZE, TREE_SIZE * 256 / 1024 / 1024);
        TREE_ROOT = buildTree(0, TREE_SIZE);

        Runtime rt = Runtime.getRuntime();
        System.gc(); Thread.sleep(200);
        System.out.printf("%nHeap used: %,d MB / %,d MB%n",
            (rt.totalMemory() - rt.freeMemory()) >> 20, rt.maxMemory() >> 20);
        System.out.println();
        System.out.println("Take a heap dump now:");
        System.out.println("  jcmd $(pgrep -f DeepObjectChain) GC.heap_dump heap.hprof");
        System.out.println();
        System.out.println("Note: the dominator tree for a linked list is linear.");
        System.out.println("  LINKED_LIST_HEAD.retainedSize ≈ entire list");
        System.out.println("  TREE_ROOT.retainedSize        ≈ entire tree");
        System.out.println("  Each node's retained size decreases as you go deeper.");

        System.out.println(LINKED_LIST_HEAD.id + " " + TREE_ROOT.value); // prevent optimizer
        Thread.sleep(Long.MAX_VALUE);
    }
}
