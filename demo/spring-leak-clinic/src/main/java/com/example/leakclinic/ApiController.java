package com.example.leakclinic;

import org.springframework.web.bind.annotation.*;
import java.util.Map;

/**
 * REST controller that exercises every leak pattern.
 *
 * The run-and-dump.sh script curls these endpoints to fill the heap before
 * capturing a dump. Each endpoint is small enough to show on a single slide.
 */
@RestController
@RequestMapping("/api")
public class ApiController {

    private final EventLog eventLog;
    private final SessionStore sessionStore;
    private final RequestTracker requestTracker;
    private final NotificationHub notificationHub;
    private final ReportEngine reportEngine;

    public ApiController(EventLog eventLog,
                         SessionStore sessionStore,
                         RequestTracker requestTracker,
                         NotificationHub notificationHub,
                         ReportEngine reportEngine) {
        this.eventLog = eventLog;
        this.sessionStore = sessionStore;
        this.requestTracker = requestTracker;
        this.notificationHub = notificationHub;
        this.reportEngine = reportEngine;
    }

    // ── Leak 1: Unbounded event log ─────────────────────────────────────────
    @PostMapping("/events")
    public Map<String, Object> logEvent(@RequestParam(defaultValue = "api") String source) {
        eventLog.log(source);
        return Map.of("status", "logged", "totalEvents", eventLog.size());
    }

    // ── Leak 2: Session cache without eviction ──────────────────────────────
    @PostMapping("/sessions/{userId}")
    public Map<String, Object> login(@PathVariable String userId) {
        sessionStore.login(userId);
        return Map.of("status", "created", "totalSessions", sessionStore.size());
    }

    // ── Leak 3: ThreadLocal never cleared ───────────────────────────────────
    @PostMapping("/track")
    public Map<String, Object> track(@RequestParam(defaultValue = "req") String requestId) {
        requestTracker.track(requestId);
        return Map.of("status", "tracked",
                       "threadEntries", requestTracker.currentThreadEntries());
    }

    // ── Leak 4: Listener/callback leak ──────────────────────────────────────
    @PostMapping("/subscribe/{subscriberId}")
    public Map<String, Object> subscribe(@PathVariable String subscriberId) {
        notificationHub.subscribe(subscriberId);
        return Map.of("status", "subscribed",
                       "totalListeners", notificationHub.listenerCount());
    }

    // ── Leak 5+6: Duplicate strings + oversized collections ─────────────────
    @PostMapping("/reports")
    public Map<String, Object> generateReport(@RequestParam(defaultValue = "r") String id) {
        reportEngine.generate(id);
        return Map.of("status", "generated", "totalReports", reportEngine.reportCount());
    }

    // ── Convenience: trigger all leaks in one call ──────────────────────────
    @PostMapping("/warm-up")
    public Map<String, Object> warmUp(@RequestParam(defaultValue = "50") int rounds) {
        for (int i = 0; i < rounds; i++) {
            eventLog.log("warmup-" + i);
            sessionStore.login("user-" + i);
            requestTracker.track("req-" + i);
            notificationHub.subscribe("sub-" + i);
            reportEngine.generate("rpt-" + i);
        }
        return Map.of(
            "rounds", rounds,
            "events", eventLog.size(),
            "sessions", sessionStore.size(),
            "listeners", notificationHub.listenerCount(),
            "reports", reportEngine.reportCount()
        );
    }

    // ── Health check ────────────────────────────────────────────────────────
    @GetMapping("/health")
    public Map<String, String> health() {
        return Map.of("status", "UP");
    }
}
