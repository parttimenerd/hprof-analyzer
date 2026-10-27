package com.example.leakclinic;

import org.springframework.stereotype.Component;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

/**
 * LEAK 2 — Session cache with no eviction.
 *
 * Every "login" creates a session holding user profile data (including a
 * simulated profile image as a byte array). Sessions are put into a map
 * but never removed — simulating a missing logout/TTL eviction.
 *
 * What you'll find in the hprof-analyzer report:
 *   • Leak Suspects  → ConcurrentHashMap retaining thousands of sessions
 *   • Top Consumers  → UserSession + byte[] near the top
 */
@Component
public class SessionStore {

    // BUG: sessions are added but never removed or expired
    private static final Map<String, UserSession> SESSIONS = new ConcurrentHashMap<>();

    /** Simulate a user login that caches a session with profile data. */
    public void login(String userId) {
        SESSIONS.put(userId, new UserSession(userId));
    }

    public int size() {
        return SESSIONS.size();
    }

    /** Inner class — a realistic session holding user data + a "profile image". */
    public static class UserSession {
        private final String userId;
        private final String displayName;
        private final String email;
        private final String role;
        private final byte[] profileImage;      // simulated avatar, ~20 KB
        private final long createdAt;
        private final Map<String, String> preferences;

        UserSession(String userId) {
            this.userId = userId;
            this.displayName = "User " + userId;
            this.email = userId + "@example.com";
            this.role = new String("ROLE_USER");     // intentionally not interned (Leak 5)
            this.profileImage = new byte[20_000];    // ~20 KB avatar
            this.createdAt = System.currentTimeMillis();
            this.preferences = new ConcurrentHashMap<>();
            this.preferences.put(new String("theme"), new String("dark"));
            this.preferences.put(new String("locale"), new String("en_US"));
            this.preferences.put(new String("timezone"), new String("UTC"));
        }
    }
}
