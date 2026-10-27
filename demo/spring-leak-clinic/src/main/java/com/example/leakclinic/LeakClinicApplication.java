package com.example.leakclinic;

import org.springframework.boot.SpringApplication;
import org.springframework.boot.autoconfigure.SpringBootApplication;

/**
 * Spring Leak Clinic — a demo app with intentional memory leaks.
 *
 * Each leak is designed to light up a different section of hprof-analyzer's report:
 *
 *   Leak 1  EventLog         → Leak Suspects, Top Consumers  (unbounded static list)
 *   Leak 2  SessionStore     → Leak Suspects                 (cache without eviction)
 *   Leak 3  RequestTracker   → Threads section               (ThreadLocal not cleared)
 *   Leak 4  NotificationHub  → Leak Suspects                 (listener/callback leak)
 *   Leak 5  ReportEngine     → Duplicate Strings report      (non-interned copies)
 *   Leak 6  ReportEngine     → Collections analysis          (oversized empty maps)
 *   Leak 7  application.properties → Secret Detection        (API keys on the heap)
 *
 * Usage:
 *   mvn package -DskipTests
 *   java -Xmx512m -jar target/spring-leak-clinic-1.0.0.jar
 *   # then curl the endpoints, take a heap dump, analyze with hprof-analyzer
 *   # see run-and-dump.sh for the full automated workflow
 */
@SpringBootApplication
public class LeakClinicApplication {

    public static void main(String[] args) {
        SpringApplication.run(LeakClinicApplication.class, args);
    }
}
