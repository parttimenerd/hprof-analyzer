//! Sensitive-data detection over a pre-built string-values map.
//!
//! Pattern sources:
//! - **betterleaks** (MIT License, <https://github.com/betterleaks/betterleaks>):
//!   463 service-specific rules compiled from `config/betterleaks.toml` at build time.
//!   Standalone rules (258) match the full string value; context-dependent rules (205)
//!   also require the holding field's name to match a service keyword.
//! - **EXTRA_PATTERN_SPECS**: heap-dump-specific patterns added here — database connection
//!   URLs (JDBC, PostgreSQL, MySQL, MongoDB, Redis, AMQP), JWT tokens, HTTP Basic auth,
//!   URL query parameters, Spring-style property values, high-entropy hex secrets, and PEM headers.
//!
//! One pass builds `HashMap<dense_idx, String>` (via `ReplCache::build_string_values`).
//! A second pass builds `HashMap<dense_idx, Vec<(owner_class, field_label)>>` (via
//! `ReplCache::build_string_referrers`), which also handles Map/Entry structures by using
//! the map key's string text as the field label — covering `HashMap`, `Properties`,
//! `ConcurrentHashMap`, and similar containers.
//! Then every pattern is applied in-memory — no additional file I/O.

use regex::RegexBuilder;
use std::collections::HashMap;

/// A single detected secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    /// Human-readable category label.
    pub category: &'static str,
    /// The raw matched string value.
    pub value: String,
    /// Where this String was referenced from: (owner_class_name, field_name).
    /// Empty when attribution was not requested or the referrer was not found.
    pub locations: Vec<(String, String)>,
}

/// A compiled pattern set. Build once, reuse across scans.
pub struct SecretPatterns {
    /// Standalone patterns: value matched in full regardless of field name.
    standalone: Vec<CompiledPattern>,
    /// Context patterns: field name must match keyword AND value must match value regex.
    context: Vec<CompiledContextPattern>,
}

struct CompiledPattern {
    category: &'static str,
    re: regex::Regex,
}

struct CompiledContextPattern {
    category: &'static str,
    keyword_re: regex::Regex,
    value_re: regex::Regex,
}

// Standalone patterns generated at build time from config/betterleaks.toml.
// betterleaks contributors, MIT License — https://github.com/betterleaks/betterleaks
// Additional heap-specific patterns (JDBC URLs, bearer tokens, credit cards) follow.
include!(concat!(env!("OUT_DIR"), "/secret_patterns.rs"));

// Context-dependent patterns: (category, keyword_regex, value_regex).
// Matched when a field name matches the keyword AND the value matches the value regex.
include!(concat!(env!("OUT_DIR"), "/context_patterns.rs"));

// Extra patterns not in betterleaks that are common in Java heap dumps.
// These cover database connection strings (credentials embedded in the URL), JWT tokens,
// HTTP Basic auth headers, URL query parameters, and common Java property formats.
// betterleaks patterns (MIT License) cover service-specific keys; these cover generic formats.
#[rustfmt::skip]
static EXTRA_PATTERN_SPECS: &[(&str, &str)] = &[
    // JDBC URLs with embedded credentials
    ("JDBC URL with credentials",       r"jdbc:.*[;?&][Pp]assword=[^;?& ]+"),
    ("JDBC URL with credentials",       r"jdbc:.*[;?&][Pp]asswd=[^;?& ]+"),
    // Database connection URLs with userinfo (postgres://, mysql://, mongodb://, redis://, amqp://)
    ("PostgreSQL connection URL",       r"postgres(?:ql)?://[^:/@]+:[^/@]{6,}@[^ ]+"),
    ("MySQL connection URL",            r"mysql://[^:/@]+:[^/@]{6,}@[^ ]+"),
    ("MongoDB connection URL",          r"mongodb(?:\+srv)?://[^:/@]+:[^/@]{6,}@[^ ]+"),
    ("Redis connection URL",            r"redis://:[A-Za-z0-9._\-~!$&'()*+,;=:]{6,}@[^ ]+"),
    ("AMQP/RabbitMQ connection URL",    r"amqps?://[^:/@]+:[^/@]{6,}@[^ ]+"),
    ("HTTP URL with embedded credentials", r"https?://[^:/@]+:[^/@]{6,}@[^ ]+"),
    // JWT tokens (header.payload.signature — all base64url segments)
    ("JWT token",                       r"eyJ[A-Za-z0-9_\-]+\.eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+"),
    // JWT / OAuth bearer tokens as bare strings
    ("Bearer token",                    r"Bearer [A-Za-z0-9._~+/=\-]{20,}"),
    // HTTP Basic auth header value (base64-encoded user:pass)
    ("HTTP Basic auth header",          r"Basic [A-Za-z0-9+/]{16,}={0,2}"),
    // URL query parameters carrying credentials
    ("URL with API key param",          r".*[?&](?:api[_-]?key|apikey|api_secret)=[^ &]{8,}"),
    ("URL with credential param",       r".*[?&](?:password|passwd|secret|token|access_token)=[^ &]{6,}"),
    // Credit card numbers (space or dash separated)
    ("Credit card number",              r"[0-9]{4}[- ][0-9]{4}[- ][0-9]{4}[- ][0-9]{4}"),
    // Spring-style property values: password=, Password:, etc.
    ("Password property value",         r"[Pp]assword[= :]+[^ ]{6,}"),
    // Generic secret= / secret: property values
    ("Secret property value",           r"[Ss]ecret[= :]+[^ ]{6,}"),
    // Generic token= / token: / api_token= etc.
    ("Token property value",            r"(?:[Aa]pi[_-]?)?[Tt]oken[= :]+[A-Za-z0-9._\-]{8,}"),
    // Generic api_key= / apiKey= etc.
    ("API key property value",          r"(?:[Aa]pi[_.-]?)?[Kk]ey[= :]+[A-Za-z0-9._\-]{8,}"),
    // Generic high-entropy hex secrets (32+ hex chars)
    ("High-entropy hex secret",         r"[0-9a-f]{32,}"),
    // PEM private key / certificate headers (heap strings often contain just the header line)
    ("Private key / certificate",       r"-----BEGIN [A-Z ]+ KEY-----"),
    // Spring Security {noop} prefix — explicitly marks a plaintext (unencrypted) password
    ("Spring Security plaintext password", r"\{noop\}.+"),
    // Spring Security delegating-password-encoder hashes — still a stored credential value
    ("Spring Security encoded password",   r"\{(?:bcrypt|pbkdf2|scrypt|argon2|sha256)\}\$.{10,}"),
];

impl Default for SecretPatterns {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretPatterns {
    /// Compile all patterns. Panics only if a built-in pattern is malformed (a bug).
    pub fn new() -> Self {
        let standalone = PATTERN_SPECS
            .iter()
            .chain(EXTRA_PATTERN_SPECS.iter())
            .filter_map(|&(category, pat)| {
                let anchored = format!("^(?:{pat})$");
                // Some betterleaks patterns compile to very large automata; skip those
                // rather than panicking. 64 MB is generous for a single pattern.
                match RegexBuilder::new(&anchored)
                    .size_limit(64 * 1024 * 1024)
                    .build()
                {
                    Ok(re) => Some(CompiledPattern { category, re }),
                    Err(_) => None,
                }
            })
            .collect();
        let context = CONTEXT_PATTERN_SPECS
            .iter()
            .filter_map(|&(category, kw_pat, val_pat)| {
                let kw_anchored = format!("(?i)^(?:{kw_pat})$");
                let val_anchored = format!("^(?:{val_pat})$");
                let keyword_re = RegexBuilder::new(&kw_anchored)
                    .size_limit(64 * 1024 * 1024)
                    .build()
                    .ok()?;
                let value_re = RegexBuilder::new(&val_anchored)
                    .size_limit(64 * 1024 * 1024)
                    .build()
                    .ok()?;
                Some(CompiledContextPattern {
                    category,
                    keyword_re,
                    value_re,
                })
            })
            .collect();
        Self {
            standalone,
            context,
        }
    }

    /// Scan `string_values` (dense_idx → decoded string) for all patterns.
    /// `attribution` maps the same dense_idx to `[(owner_class, field_name)]` referrers;
    /// pass an empty map when attribution is not needed.
    ///
    /// Runs both standalone patterns (value-only) and context patterns (field-name-gated).
    /// Deduplicates by (category, value). Returns findings in scan order.
    pub fn scan(
        &self,
        string_values: &HashMap<u32, String>,
        attribution: &HashMap<u32, Vec<(String, String)>>,
    ) -> Vec<SecretFinding> {
        let mut seen: HashMap<(&'static str, &str), ()> = HashMap::new();
        let mut findings: Vec<SecretFinding> = Vec::new();

        for (dense_idx, value) in string_values {
            let locations = attribution.get(dense_idx).cloned().unwrap_or_default();

            // Standalone patterns: match value alone.
            for pattern in &self.standalone {
                if pattern.re.is_match(value) {
                    if seen
                        .insert((pattern.category, value.as_str()), ())
                        .is_none()
                    {
                        findings.push(SecretFinding {
                            category: pattern.category,
                            value: value.clone(),
                            locations: locations.clone(),
                        });
                    }
                }
            }

            // Context patterns: match field name against keyword, then value against value regex.
            if !self.context.is_empty() && !locations.is_empty() {
                for pattern in &self.context {
                    if !pattern.value_re.is_match(value) {
                        continue;
                    }
                    // Check if any referrer field name matches the keyword.
                    let matched_locs: Vec<(String, String)> = locations
                        .iter()
                        .filter(|(_, field)| pattern.keyword_re.is_match(field))
                        .cloned()
                        .collect();
                    if matched_locs.is_empty() {
                        continue;
                    }
                    if seen
                        .insert((pattern.category, value.as_str()), ())
                        .is_none()
                    {
                        findings.push(SecretFinding {
                            category: pattern.category,
                            value: value.clone(),
                            locations: matched_locs,
                        });
                    }
                }
            }
        }

        findings
    }
}

/// The PATTERN_SPECS slice (generated from betterleaks.toml) plus EXTRA_PATTERN_SPECS
/// are the single source of truth for both the Rust scanner and the browser JS.
/// The JS page calls `find_secrets()` which uses this directly.
#[cfg(test)]
mod tests {
    use super::*;

    fn scan(values: &[&str]) -> Vec<SecretFinding> {
        let map: HashMap<u32, String> = values
            .iter()
            .enumerate()
            .map(|(i, s)| (i as u32, s.to_string()))
            .collect();
        SecretPatterns::new().scan(&map, &HashMap::new())
    }

    fn has_category(findings: &[SecretFinding], cat: &str) -> bool {
        findings.iter().any(|f| f.category == cat)
    }

    // ── JDBC URLs ────────────────────────────────────────────────────────────

    #[test]
    fn jdbc_semicolon_password() {
        let r = scan(&["jdbc:h2:mem:petclinic;DB_CLOSE_DELAY=-1;password=petclinic123"]);
        assert!(!r.is_empty(), "expected JDBC match");
        assert_eq!(r[0].category, "JDBC URL with credentials");
        assert!(r[0].value.contains("password=petclinic123"));
    }

    #[test]
    fn jdbc_question_mark_password() {
        let r = scan(&["jdbc:postgresql://host/db?password=hunter2"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "JDBC URL with credentials");
    }

    #[test]
    fn jdbc_passwd_variant() {
        let r = scan(&["jdbc:mysql://host/db;passwd=s3cr3t"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "JDBC URL with credentials");
    }

    #[test]
    fn jdbc_no_password_clean() {
        let r = scan(&["jdbc:h2:mem:petclinic", "jdbc:postgresql://host/db?user=sa"]);
        assert!(r.is_empty(), "plain JDBC URL should not match");
    }

    // ── API keys ─────────────────────────────────────────────────────────────

    #[test]
    fn anthropic_key_matches() {
        // betterleaks anthropic-api-key: sk-ant-api03-<93 chars>AA
        let key = format!("sk-ant-api03-{}AA", "a".repeat(93));
        let r = scan(&[&key]);
        assert!(!r.is_empty(), "expected Anthropic key match");
        assert_eq!(r[0].category, "An Anthropic API Key");
    }

    #[test]
    fn huggingface_token_matches() {
        // betterleaks: hf_(?i:[a-z]{34}) — exactly 34 lowercase letters
        let r = scan(&["hf_abcdefghijklmnopqrstuvwxyzabcdefgh"]);
        assert!(!r.is_empty(), "expected HuggingFace token match");
        assert_eq!(r[0].category, "A Hugging Face Access token");
    }

    #[test]
    fn aws_access_key_matches() {
        let r = scan(&["AKIAIOSFODNN7EXAMPLE"]);
        assert!(!r.is_empty());
        assert_eq!(
            r[0].category,
            "An AWS access key ID paired with a secret access key"
        );
    }

    #[test]
    fn aws_key_wrong_prefix() {
        let r = scan(&["AXIAIOSFODNN7EXAMPLE"]);
        assert!(r.is_empty());
    }

    // ── Bearer tokens ────────────────────────────────────────────────────────

    #[test]
    fn bearer_token_matches() {
        let r = scan(&["Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.payload"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "Bearer token");
    }

    #[test]
    fn bearer_token_too_short() {
        let r = scan(&["Bearer short"]);
        assert!(r.is_empty());
    }

    // ── Credit cards ─────────────────────────────────────────────────────────

    #[test]
    fn credit_card_space_separated() {
        let r = scan(&["4111 1111 1111 1111"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "Credit card number");
    }

    #[test]
    fn credit_card_dash_separated() {
        let r = scan(&["4111-1111-1111-1111"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "Credit card number");
    }

    #[test]
    fn credit_card_no_separator() {
        let r = scan(&["4111111111111111"]);
        assert!(r.is_empty(), "unseparated digits should not match");
    }

    // ── PEM headers ──────────────────────────────────────────────────────────

    #[test]
    fn pem_private_key() {
        // betterleaks private-key pattern requires the full PEM block, not just the header.
        // For heap strings the header alone is what's stored; check it's found by our
        // EXTRA_PATTERN_SPECS fallback (Private key / certificate).
        let r = scan(&["-----BEGIN RSA PRIVATE KEY-----"]);
        // May match either betterleaks or extra patterns
        assert!(!r.is_empty(), "expected PEM header match");
    }

    #[test]
    fn pem_ec_key() {
        let r = scan(&["-----BEGIN EC PRIVATE KEY-----"]);
        assert!(!r.is_empty(), "expected EC PEM header match");
    }

    // ── Password property values ─────────────────────────────────────────────

    #[test]
    fn password_equals() {
        // betterleaks generic-password category
        let r = scan(&["password=hunter2abc"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "Hardcoded password literal");
    }

    #[test]
    fn password_colon() {
        let r = scan(&["Password: secretvalue"]);
        assert!(!r.is_empty());
    }

    #[test]
    fn password_too_short() {
        // generic-password requires at least 5 chars after the separator
        let r = scan(&["password=abc"]);
        assert!(r.is_empty());
    }

    // ── Deduplication ────────────────────────────────────────────────────────

    #[test]
    fn deduplicates_same_value() {
        // Same string appears twice in the map under different dense indices.
        let aws_key = "AKIAIOSFODNN7EXAMPLE";
        let map: HashMap<u32, String> =
            [(0u32, aws_key.to_string()), (1u32, aws_key.to_string())].into();
        let findings = SecretPatterns::new().scan(&map, &HashMap::new());
        assert_eq!(findings.len(), 1, "duplicate values should be deduplicated");
    }

    // ── Clean strings ────────────────────────────────────────────────────────

    #[test]
    fn clean_strings_no_findings() {
        let r = scan(&[
            "Hello, World!",
            "java.lang.String",
            "SELECT * FROM users",
            "http://example.com",
            "jdbc:h2:mem:test",
            "2024-01-01",
        ]);
        assert!(r.is_empty(), "no secrets in clean strings");
    }

    // ── Multiple matches in one scan ─────────────────────────────────────────

    #[test]
    fn multiple_secret_types() {
        let r = scan(&[
            "AKIAIOSFODNN7EXAMPLE",
            "jdbc:h2:mem:db;password=petclinic123",
            "not a secret",
        ]);
        assert!(has_category(
            &r,
            "An AWS access key ID paired with a secret access key"
        ));
        assert!(has_category(&r, "JDBC URL with credentials"));
        assert_eq!(r.len(), 2);
    }

    // ── GitHub tokens ─────────────────────────────────────────────────────────

    #[test]
    fn github_pat_classic() {
        let r = scan(&["ghp_FAKE000000000000000000000000000000XX"]);
        assert!(has_category(&r, "A GitHub Personal Access Token"));
    }

    #[test]
    fn github_app_token() {
        let r = scan(&["ghs_FAKE000000000000000000000000000000XX"]);
        assert!(has_category(&r, "A GitHub App Token"));
    }

    // ── Google API keys ───────────────────────────────────────────────────────

    #[test]
    fn google_api_key() {
        let r = scan(&["AIzaFAKE00000000000000000000000000000XX"]);
        assert!(has_category(&r, "A GCP API key"));
    }

    // ── Slack tokens ─────────────────────────────────────────────────────────

    #[test]
    fn slack_bot_token() {
        // betterleaks xoxb-{10,13}-{10,13}...
        let r = scan(&["xoxb-1234567890-1234567890-aaaaaaaaaaaaaaaaaaaaaaaa"]);
        assert!(has_category(&r, "A Slack Bot token"));
    }

    #[test]
    fn slack_user_token() {
        // betterleaks: xox[pe](?:-[0-9]{10,13}){3}-[a-zA-Z0-9-]{28,34}
        let token = "xoxp-1234567890-1234567890-1234567890-abcdefghijklmnopqrstuvwxyz12";
        let r = scan(&[token]);
        assert!(
            has_category(&r, "Found a Slack User token"),
            "findings: {r:?}"
        );
    }

    // ── High-entropy hex ─────────────────────────────────────────────────────

    #[test]
    fn high_entropy_hex_secret() {
        let r = scan(&["a3f1b2c4d5e6f7a8b9c0d1e2f3a4b5c6"]);
        assert!(has_category(&r, "High-entropy hex secret"));
    }

    #[test]
    fn short_hex_not_flagged() {
        let r = scan(&["deadbeef"]);
        assert!(!has_category(&r, "High-entropy hex secret"));
    }

    // ── Connection URLs ──────────────────────────────────────────────────────

    #[test]
    fn postgres_url_with_password() {
        let r = scan(&["postgresql://admin:s3cr3t@db.example.com/mydb"]);
        assert!(
            has_category(&r, "PostgreSQL connection URL"),
            "findings: {r:?}"
        );
    }

    #[test]
    fn mysql_url_with_password() {
        let r = scan(&["mysql://root:hunter2@localhost/app"]);
        assert!(has_category(&r, "MySQL connection URL"), "findings: {r:?}");
    }

    #[test]
    fn mongodb_url_with_password() {
        let r = scan(&["mongodb://user:pass1234@mongo.internal:27017/dbname"]);
        assert!(
            has_category(&r, "MongoDB connection URL"),
            "findings: {r:?}"
        );
    }

    #[test]
    fn redis_url_with_password() {
        let r = scan(&["redis://:secretpassword@redis.internal:6379/0"]);
        assert!(has_category(&r, "Redis connection URL"), "findings: {r:?}");
    }

    #[test]
    fn amqp_url_with_password() {
        let r = scan(&["amqp://guest:guest123@rabbitmq.internal:5672/vhost"]);
        assert!(
            has_category(&r, "AMQP/RabbitMQ connection URL"),
            "findings: {r:?}"
        );
    }

    #[test]
    fn http_url_no_credentials_clean() {
        let r = scan(&[
            "https://example.com/api/v1/users",
            "http://localhost:8080/health",
        ]);
        assert!(!has_category(&r, "HTTP URL with embedded credentials"));
    }

    // ── JWT tokens ───────────────────────────────────────────────────────────

    #[test]
    fn jwt_token_matches() {
        let r = scan(&[
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
        ]);
        assert!(has_category(&r, "JWT token"), "findings: {r:?}");
    }

    // ── HTTP Basic auth ──────────────────────────────────────────────────────

    #[test]
    fn http_basic_auth_matches() {
        let r = scan(&["Basic dXNlcjpwYXNzd29yZA=="]);
        assert!(
            has_category(&r, "HTTP Basic auth header"),
            "findings: {r:?}"
        );
    }

    #[test]
    fn http_basic_auth_too_short() {
        let r = scan(&["Basic dXNlcg=="]);
        assert!(!has_category(&r, "HTTP Basic auth header"));
    }

    // ── URL query parameters ─────────────────────────────────────────────────

    #[test]
    fn url_with_api_key_param() {
        let r = scan(&["https://api.example.com/data?api_key=abc123def456xyz"]);
        assert!(
            has_category(&r, "URL with API key param"),
            "findings: {r:?}"
        );
    }

    #[test]
    fn url_with_password_param() {
        let r = scan(&["https://example.com/login?username=admin&password=supersecret"]);
        assert!(
            has_category(&r, "URL with credential param"),
            "findings: {r:?}"
        );
    }

    // ── Spring Security password encoding ────────────────────────────────────

    #[test]
    fn spring_noop_password() {
        let r = scan(&["{noop}myplaintextpassword"]);
        assert!(
            has_category(&r, "Spring Security plaintext password"),
            "findings: {r:?}"
        );
    }

    #[test]
    fn spring_bcrypt_password() {
        let r = scan(&["{bcrypt}$2a$10$EixZaYVK1fsbw1ZfbX3OXePaWxn96p36WQoeG6Lruj3vjPGga31lW"]);
        assert!(
            has_category(&r, "Spring Security encoded password"),
            "findings: {r:?}"
        );
    }

    #[test]
    fn spring_noop_too_short() {
        // {noop} alone with empty value shouldn't match
        let r = scan(&["{noop}"]);
        assert!(!has_category(&r, "Spring Security plaintext password"));
    }

    // ── Integration: real Spring PetClinic fixture ────────────────────────────
    // Skipped automatically when the fixture is absent (CI before gen-spring-fixture.sh runs).

    #[test]
    fn spring_petclinic_fixture_contains_secrets() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/samples/spring-petclinic-h2-ai.hprof.gz");
        if !fixture.exists() {
            eprintln!("SKIP: fixture not found at {}", fixture.display());
            return;
        }

        let source = crate::HprofSource::Path(fixture.to_string_lossy().into_owned());
        let cache = crate::query::run::ReplCache::build(&source, true).expect("ReplCache::build");
        let string_values = cache.build_string_values().expect("build_string_values");

        let findings =
            SecretPatterns::new().scan(&string_values, &std::collections::HashMap::new());

        let categories: std::collections::HashSet<&str> =
            findings.iter().map(|f| f.category).collect();

        assert!(
            categories.contains("JDBC URL with credentials"),
            "expected to find the JDBC URL with password; findings: {findings:?}"
        );
        // The fixture contains a sk-demo-... key; betterleaks has no generic sk- rule,
        // but our EXTRA_PATTERN_SPECS Password/JDBC patterns should still fire.
        // We only assert the JDBC finding here since we removed the generic sk- pattern.

        let jdbc = findings
            .iter()
            .find(|f| f.category == "JDBC URL with credentials")
            .unwrap();
        assert!(
            jdbc.value.contains("password=petclinic123"),
            "JDBC value unexpected: {}",
            jdbc.value
        );
    }
}
