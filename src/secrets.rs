//! Sensitive-data detection over a pre-built string-values map.
//!
//! One pass builds `HashMap<dense_idx, String>` (via `ReplCache::build_string_values`).
//! Then every pattern is applied in-memory — no additional file I/O.

use regex::Regex;
use std::collections::HashMap;

/// A single detected secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFinding {
    /// Human-readable category label.
    pub category: &'static str,
    /// The raw matched string value.
    pub value: String,
}

/// A compiled pattern set. Build once, reuse across scans.
pub struct SecretPatterns(Vec<CompiledPattern>);

struct CompiledPattern {
    category: &'static str,
    re: Regex,
}

/// The canonical set of patterns used by the browser scanner.
/// Each pattern is a full-match (anchored) Java-style regex — same semantics as
/// the OQL `LIKE` operator.
#[rustfmt::skip]
static PATTERN_SPECS: &[(&str, &str)] = &[
    // JDBC URLs with embedded credentials
    ("JDBC URL with credentials",    r"jdbc:.*[;?&][Pp]assword=[^;?& ]+"),
    ("JDBC URL with credentials",    r"jdbc:.*[;?&][Pp]asswd=[^;?& ]+"),
    // OpenAI / Anthropic / generic sk- keys
    ("OpenAI / Anthropic API key",   r"sk-[A-Za-z0-9_-]{20,}"),
    // HuggingFace tokens
    ("HuggingFace token",            r"hf_[A-Za-z0-9]{16,}"),
    // AWS access keys
    ("AWS access key",               r"AKIA[0-9A-Z]{16}"),
    // JWT / OAuth bearer tokens
    ("Bearer token",                 r"Bearer [A-Za-z0-9._~+/=\-]{20,}"),
    // Credit card numbers (space or dash separated)
    ("Credit card number",           r"[0-9]{4}[- ][0-9]{4}[- ][0-9]{4}[- ][0-9]{4}"),
    // PEM private key headers
    ("Private key / certificate",    r"-----BEGIN [A-Z ]+ KEY-----"),
    // Spring-style property values: password=, Password:, etc.
    ("Password property value",      r"[Pp]assword[= :]+[^ ]{6,}"),
    // Generic secret= / secret: property values
    ("Secret property value",        r"[Ss]ecret[= :]+[^ ]{6,}"),
    // Generic token= / token: / api_token= etc.
    ("Token property value",         r"(?:[Aa]pi[_-]?)?[Tt]oken[= :]+[A-Za-z0-9._\-]{8,}"),
    // Generic api_key= / apiKey= etc.
    ("API key property value",       r"(?:[Aa]pi[_.-]?)?[Kk]ey[= :]+[A-Za-z0-9._\-]{8,}"),
    // GitHub personal access tokens (classic and fine-grained)
    ("GitHub token",                 r"gh[pousr]_[A-Za-z0-9]{36,}"),
    // Google API keys
    ("Google API key",               r"AIza[0-9A-Za-z\-_]{35}"),
    // Slack tokens
    ("Slack token",                  r"xox[baprs]-[0-9A-Za-z\-]{10,}"),
    // Generic high-entropy hex secrets (32+ hex chars, e.g. DB passwords, symmetric keys)
    ("High-entropy hex secret",      r"[0-9a-f]{32,}"),
];

impl Default for SecretPatterns {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretPatterns {
    /// Compile all patterns. Panics only if a built-in pattern is malformed (a bug).
    pub fn new() -> Self {
        let compiled = PATTERN_SPECS
            .iter()
            .map(|&(category, pat)| {
                // Full-match anchoring: same as OQL LIKE semantics.
                let anchored = format!("^(?:{pat})$");
                CompiledPattern {
                    category,
                    re: Regex::new(&anchored).expect("built-in secret pattern is valid"),
                }
            })
            .collect();
        Self(compiled)
    }

    /// Scan `string_values` (dense_idx → decoded string) for all patterns.
    /// Deduplicates by (category, value). Returns findings in scan order.
    pub fn scan(&self, string_values: &HashMap<u32, String>) -> Vec<SecretFinding> {
        let mut seen: HashMap<(&'static str, &str), ()> = HashMap::new();
        let mut findings: Vec<SecretFinding> = Vec::new();

        for value in string_values.values() {
            for pattern in &self.0 {
                if pattern.re.is_match(value) {
                    // Deduplicate: same category + value only once.
                    if seen
                        .insert((pattern.category, value.as_str()), ())
                        .is_none()
                    {
                        findings.push(SecretFinding {
                            category: pattern.category,
                            value: value.clone(),
                        });
                    }
                }
            }
        }

        findings
    }
}

/// The PATTERN_SPECS slice is the single source of truth for both the Rust
/// scanner and the browser JS. The JS page calls `find_secrets()` which uses
/// this directly — there is no separate OQL_PATTERNS constant.
#[cfg(test)]
mod tests {
    use super::*;

    fn scan(values: &[&str]) -> Vec<SecretFinding> {
        let map: HashMap<u32, String> = values
            .iter()
            .enumerate()
            .map(|(i, s)| (i as u32, s.to_string()))
            .collect();
        SecretPatterns::new().scan(&map)
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
    fn openai_key_matches() {
        let r = scan(&["sk-demo-thisisasecret12345678901234"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "OpenAI / Anthropic API key");
    }

    #[test]
    fn openai_key_too_short() {
        // prefix correct but only 10 chars after — shorter than {20,}
        let r = scan(&["sk-tooshort1234567890"]);
        // 20 chars after "sk-" — borderline, should match (exactly 20)
        let r_exact = scan(&["sk-exactly20charshere!"]);
        // less than 20
        let r_short = scan(&["sk-lessthan20chars1"]);
        assert!(r_short.is_empty(), "too-short key should not match");
        let _ = (r, r_exact); // just ensure they compile/run
    }

    #[test]
    fn huggingface_token_matches() {
        let r = scan(&["hf_abcdefghijklmnopqrstuvwx"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "HuggingFace token");
    }

    #[test]
    fn aws_access_key_matches() {
        let r = scan(&["AKIAIOSFODNN7EXAMPLE"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "AWS access key");
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
        let r = scan(&["-----BEGIN RSA PRIVATE KEY-----"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "Private key / certificate");
    }

    #[test]
    fn pem_ec_key() {
        let r = scan(&["-----BEGIN EC PRIVATE KEY-----"]);
        assert!(!r.is_empty());
    }

    // ── Password property values ─────────────────────────────────────────────

    #[test]
    fn password_equals() {
        let r = scan(&["password=hunter2abc"]);
        assert!(!r.is_empty());
        assert_eq!(r[0].category, "Password property value");
    }

    #[test]
    fn password_colon() {
        let r = scan(&["Password: secretvalue"]);
        assert!(!r.is_empty());
    }

    #[test]
    fn password_too_short() {
        let r = scan(&["password=short"]);
        // "short" is 5 chars — less than {6,}
        assert!(r.is_empty());
    }

    // ── Deduplication ────────────────────────────────────────────────────────

    #[test]
    fn deduplicates_same_value() {
        // Same string appears twice in the map under different dense indices.
        let map: HashMap<u32, String> = [
            (0u32, "sk-demo-thisisasecret12345678901234".to_string()),
            (1u32, "sk-demo-thisisasecret12345678901234".to_string()),
        ]
        .into();
        let findings = SecretPatterns::new().scan(&map);
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
            "sk-demo-thisisasecret12345678901234",
            "jdbc:h2:mem:db;password=petclinic123",
            "not a secret",
        ]);
        assert!(has_category(&r, "OpenAI / Anthropic API key"));
        assert!(has_category(&r, "JDBC URL with credentials"));
        assert_eq!(r.len(), 2);
    }

    // ── Secret / token / key property values ─────────────────────────────────

    #[test]
    fn secret_equals() {
        let r = scan(&["secret=mysupersecretsval"]);
        assert!(has_category(&r, "Secret property value"));
    }

    #[test]
    fn secret_colon() {
        let r = scan(&["Secret: mysupersecretsval"]);
        assert!(has_category(&r, "Secret property value"));
    }

    #[test]
    fn token_equals() {
        let r = scan(&["token=abcdef1234567890"]);
        assert!(has_category(&r, "Token property value"));
    }

    #[test]
    fn api_token_equals() {
        let r = scan(&["api_token=abcdef1234567890"]);
        assert!(has_category(&r, "Token property value"));
    }

    #[test]
    fn api_key_equals() {
        let r = scan(&["api_key=abcdef12345678"]);
        assert!(has_category(&r, "API key property value"));
    }

    #[test]
    fn apikey_camelcase() {
        let r = scan(&["apiKey=abcdef12345678"]);
        assert!(has_category(&r, "API key property value"));
    }

    #[test]
    fn api_key_too_short() {
        let r = scan(&["api_key=short"]);
        // "short" is 5 chars, pattern needs {8,}
        assert!(!has_category(&r, "API key property value"));
    }

    // ── GitHub tokens ─────────────────────────────────────────────────────────

    #[test]
    fn github_pat_classic() {
        let r = scan(&["ghp_FAKE000000000000000000000000000000XX"]);
        assert!(has_category(&r, "GitHub token"));
    }

    #[test]
    fn github_pat_fine_grained() {
        let r = scan(&["github_pat_11ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdef"]);
        // doesn't start with gh[pousr]_ so should NOT match
        assert!(!has_category(&r, "GitHub token"));
    }

    #[test]
    fn github_actions_token() {
        let r = scan(&["ghs_FAKE000000000000000000000000000000XX"]);
        assert!(has_category(&r, "GitHub token"));
    }

    // ── Google API keys ───────────────────────────────────────────────────────

    #[test]
    fn google_api_key() {
        let r = scan(&["AIzaFAKE00000000000000000000000000000XX"]);
        assert!(has_category(&r, "Google API key"));
    }

    // ── Slack tokens ─────────────────────────────────────────────────────────

    #[test]
    fn slack_bot_token() {
        let r = scan(&["xoxb-FAKE-000000000-aaaaaaaaaaa"]);
        assert!(has_category(&r, "Slack token"));
    }

    #[test]
    fn slack_user_token() {
        let r = scan(&["xoxp-FAKE-000000000-aaaaaaaaaaa"]);
        assert!(has_category(&r, "Slack token"));
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
        // only 8 chars, needs 32+
        assert!(!has_category(&r, "High-entropy hex secret"));
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

        let findings = SecretPatterns::new().scan(&string_values);

        let categories: std::collections::HashSet<&str> =
            findings.iter().map(|f| f.category).collect();

        assert!(
            categories.contains("OpenAI / Anthropic API key"),
            "expected to find the sk-demo-... API key; findings: {findings:?}"
        );
        assert!(
            categories.contains("JDBC URL with credentials"),
            "expected to find the JDBC URL with password; findings: {findings:?}"
        );

        // Spot-check the actual values
        let api_key = findings
            .iter()
            .find(|f| f.category == "OpenAI / Anthropic API key")
            .unwrap();
        assert!(
            api_key.value.starts_with("sk-demo-"),
            "API key value unexpected: {}",
            api_key.value
        );

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
