package com.example.leakclinic;

import org.springframework.stereotype.Component;
import java.util.ArrayDeque;
import java.util.Deque;

/**
 * Fixed: bounded event log capped at MAX_EVENTS entries.
 * Oldest entries are evicted when the cap is reached.
 */
@Component
public class EventLog {

    private static final int MAX_EVENTS = 100;

    private final Deque<byte[]> events = new ArrayDeque<>();

    public synchronized void log(String source) {
        byte[] payload = new byte[50_000];
        for (int i = 0; i < payload.length; i += 64) {
            payload[i] = (byte) source.hashCode();
        }
        if (events.size() >= MAX_EVENTS) {
            events.pollFirst();
        }
        events.addLast(payload);
    }

    public synchronized int size() {
        return events.size();
    }
}
