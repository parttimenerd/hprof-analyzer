/**
 * SCENARIO 16 — Requested array size exceeds VM limit
 *
 * Run:   java 16_ArraySizeOOM.java
 *        (No -Xmx needed — this is a per-array size limit, not a total heap limit)
 *
 * OOM:   java.lang.OutOfMemoryError: Requested array size exceeds VM limit
 *
 * Root:  The JVM limits the maximum number of elements in any single array to
 *        Integer.MAX_VALUE - some small constant (typically 2 or 5, implementation-
 *        defined). Even if the heap is large enough to hold the data, an array of
 *        Integer.MAX_VALUE (2 147 483 647) elements is rejected.
 *
 *        This is NOT a memory exhaustion error. It fires even on a machine with
 *        terabytes of RAM and a multi-GB heap.
 *
 *        Common causes:
 *          - A size calculation overflows int (e.g., `new byte[n * m]` where n × m
 *            wraps around to a negative or huge positive value)
 *          - A deserialization attack / untrusted length field in a protocol
 *          - Accidentally using MAX_VALUE as "unlimited" in a buffer pre-allocation
 *
 * This error CANNOT be caught by setting a larger -Xmx — fix the size calculation.
 *
 * Fix:   Validate sizes before allocating:
 *          if (size < 0 || size > MAX_SAFE_ARRAY_SIZE) throw new IllegalArgumentException(...)
 *        Use long arithmetic for size calculations, then check before casting to int.
 *        Define MAX_SAFE_ARRAY_SIZE = Integer.MAX_VALUE - 8  (matches ArrayList/Arrays).
 */
public class _16_ArraySizeOOM {

    static final int MAX_SAFE = Integer.MAX_VALUE - 8; // same constant used by java.util.Arrays

    /** Simulates a buggy deserializer that reads an untrusted length field */
    static byte[] readPayload(int reportedLength) {
        // ❌ BAD: trusts the length from an external source without validation
        return new byte[reportedLength];

        // ✅ FIX: validate first
        // if (reportedLength < 0 || reportedLength > MAX_SAFE)
        //     throw new IllegalArgumentException("Invalid payload length: " + reportedLength);
        // return new byte[reportedLength];
    }

    public static void main(String[] args) {
        System.out.println("Attempting to allocate Integer.MAX_VALUE elements …");
        // This fires immediately — no loop needed
        byte[] buf = readPayload(Integer.MAX_VALUE);
        System.out.println("Allocated " + buf.length + " bytes"); // never reached
    }
}
