package com.example.leakclinic;

import org.springframework.stereotype.Component;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.function.Consumer;

/**
 * Fixed: listeners are stored by subscriberId so they can be explicitly
 * unsubscribed. The processing context is released when unsubscribe() is called.
 */
@Component
public class NotificationHub {

    private final Map<String, Consumer<String>> listeners = new ConcurrentHashMap<>();

    public void subscribe(String subscriberId) {
        byte[] processingContext = new byte[100_000];
        processingContext[0] = (byte) subscriberId.hashCode();

        listeners.put(subscriberId, event -> {
            int checksum = processingContext[0] + event.length();
        });
    }

    public void unsubscribe(String subscriberId) {
        listeners.remove(subscriberId);
    }

    public void publish(String event) {
        listeners.values().forEach(l -> l.accept(event));
    }

    public int listenerCount() {
        return listeners.size();
    }
}
