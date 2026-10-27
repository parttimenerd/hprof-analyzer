package com.example.leakclinic;

import org.springframework.stereotype.Component;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * Fixed:
 *  - String literals instead of new String(...) — JVM deduplication applies
 *  - Default-capacity collections instead of oversized pre-allocation
 *  - No static accumulation list; reports are not retained after generation
 */
@Component
public class ReportEngine {

    private int generated = 0;

    public void generate(String reportId) {
        List<String> statuses = new ArrayList<>();
        for (int i = 0; i < 200; i++) {
            statuses.add("ACTIVE");
            statuses.add("PENDING");
            statuses.add("COMPLETED");
            statuses.add("CANCELLED");
            statuses.add("ERROR");
            statuses.add("ROLE_USER");
            statuses.add("application/json");
            statuses.add("Content-Type");
            statuses.add("Authorization");
            statuses.add("Bearer");
        }

        Map<String, Object> cache = new HashMap<>();
        cache.put("report-" + reportId, statuses);

        List<Object> buffer = new ArrayList<>();
        buffer.add(reportId);

        // Process and discard — results are not accumulated in a static field
        generated++;
    }

    public int reportCount() {
        return generated;
    }
}
