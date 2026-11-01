//! User-extensible class classification: "which category does this class
//! belong to" (e.g. `jdk`, `framework`, `application`, or any user-defined
//! name). This generalizes the previously hardcoded JDK/non-JDK split into
//! an open-ended, ordered registry — a user can define `application`/
//! `legacy`/whatever categories they want via `.hprof-analyzer.toml`'s
//! `[[category]]` section or the `--category name=prefix` CLI flag, and
//! those are checked BEFORE the two built-in categories (`framework`,
//! `jdk`), so a user category can "claim" classes that would otherwise fall
//! into a built-in or `other`.
//!
//! Matching rule: first matching category wins, in list order. A class that
//! matches nothing gets the synthetic category name `"other"` — deliberately
//! NOT folded into `"application"`, since that would hide exactly the
//! classes a user most needs to notice (unrecognized/uncategorized code).

/// Synthetic category name for a class matching no configured/built-in
/// category. Never explicitly configurable — always the fallback.
pub const OTHER_CATEGORY: &str = "other";
/// Built-in category name for framework sentinel classes (see
/// `pass2::framework_scan`'s SENTINELS list, reused here as prefixes/exact
/// matches so both features share one source of truth for "what counts as
/// Hibernate/Spring/etc").
pub const FRAMEWORK_CATEGORY: &str = "framework";
/// Built-in category name for JDK/standard-library classes.
pub const JDK_CATEGORY: &str = "jdk";

/// One matcher within a `CategoryDef`. `Prefix` is the common, fast case
/// (no regex engine); `Regex` is an escape hatch for teams that need more
/// precise matching (e.g. "only these two sub-packages, not the whole tree").
#[derive(Debug, Clone)]
pub enum Matcher {
    Prefix(String),
    Regex(regex::Regex),
}

impl Matcher {
    fn matches(&self, pretty_class: &str) -> bool {
        match self {
            Matcher::Prefix(p) => pretty_class.starts_with(p.as_str()),
            Matcher::Regex(re) => re.is_match(pretty_class),
        }
    }
}

/// One named category: a human-chosen name plus the matchers that claim a
/// class into it. Matchers within a category are OR'd together.
#[derive(Debug, Clone)]
pub struct CategoryDef {
    pub name: String,
    pub matchers: Vec<Matcher>,
}

impl CategoryDef {
    pub fn prefix(name: &str, prefixes: &[&str]) -> CategoryDef {
        CategoryDef {
            name: name.to_string(),
            matchers: prefixes
                .iter()
                .map(|p| Matcher::Prefix(p.to_string()))
                .collect(),
        }
    }
}

/// Ordered classifier: categories are checked in list order, first match
/// wins. Built from user config (if any) followed by the two built-ins
/// (`framework`, `jdk`) — see module docs for why user categories come
/// first and why unmatched classes become `"other"`, not `"application"`.
#[derive(Debug, Clone)]
pub struct Classifier {
    categories: Vec<CategoryDef>,
}

impl Classifier {
    pub fn new(categories: Vec<CategoryDef>) -> Classifier {
        Classifier { categories }
    }

    /// Classify a pretty (dotted) class name. Returns the first matching
    /// category's name, or `OTHER_CATEGORY` if nothing matches.
    pub fn classify(&self, pretty_class: &str) -> &str {
        for cat in &self.categories {
            if cat.matchers.iter().any(|m| m.matches(pretty_class)) {
                return &cat.name;
            }
        }
        OTHER_CATEGORY
    }
}

/// The two built-in categories, checked AFTER any user-defined ones. Mirrors
/// `pass2::framework_scan::SENTINELS`'s package prefixes (kept in sync by
/// hand — framework_scan's exact-class-name sentinels are a strict subset of
/// these broader package prefixes) plus the standard JDK/platform
/// namespaces (shared with `report::build::is_stdlib_class`'s list, though
/// that helper additionally excludes primitive arrays, which have no
/// `.`-prefixed package name to classify here anyway — a prim array's
/// pretty-printed name already falls through to `other` harmlessly, since
/// classification is about understanding where APPLICATION weight sits, and
/// a handful of unclassified `int[]`/`byte[]` rows under `other` is not a
/// loss of signal).
fn builtin_categories() -> Vec<CategoryDef> {
    vec![
        CategoryDef::prefix(
            FRAMEWORK_CATEGORY,
            &[
                "org.hibernate.",
                "org.springframework.",
                "java.util.concurrent.ThreadPoolExecutor",
                "io.netty.",
                "com.zaxxer.hikari.",
            ],
        ),
        CategoryDef::prefix(
            JDK_CATEGORY,
            &[
                "java.", "javax.", "jdk.", "sun.", "com.sun.", "scala.", "kotlin.",
            ],
        ),
    ]
}

/// Build a `Classifier` from user-defined categories (already parsed, in
/// file order) plus the built-ins. User categories always evaluate first —
/// see module docs — so a user can override/narrow a built-in by defining a
/// category that matches a subset of it (e.g. claim `java.` classes into
/// their own "vendored-but-ours" category) as a deliberate power-user escape
/// hatch, not the common case.
pub fn build_classifier(user_categories: Vec<CategoryDef>) -> Classifier {
    let mut categories = user_categories;
    categories.extend(builtin_categories());
    Classifier::new(categories)
}

/// Lint: warn (to stderr; never a hard error) when a LATER user category's
/// prefixes are a strict subset of an EARLIER one's — a common config
/// mistake where the more specific category is listed second and therefore
/// never matches anything (the earlier, broader one always claims the class
/// first). Only checks `Matcher::Prefix` pairs (regex subset-checking is not
/// generally decidable without a regex-equivalence library, not worth the
/// complexity for a best-effort lint).
pub fn lint_category_order(categories: &[CategoryDef]) {
    for i in 0..categories.len() {
        for j in (i + 1)..categories.len() {
            let (earlier, later) = (&categories[i], &categories[j]);
            let earlier_prefixes: Vec<&str> = earlier
                .matchers
                .iter()
                .filter_map(|m| match m {
                    Matcher::Prefix(p) => Some(p.as_str()),
                    Matcher::Regex(_) => None,
                })
                .collect();
            for m in &later.matchers {
                if let Matcher::Prefix(later_prefix) = m {
                    if earlier_prefixes
                        .iter()
                        .any(|&ep| later_prefix.starts_with(ep) && later_prefix.len() > ep.len())
                    {
                        eprintln!(
                            "warning: category {:?} (prefix {:?}) is listed after {:?} \
                             (prefix {:?}), which already matches everything {:?} would — \
                             {:?} will never match any class. List more specific categories \
                             BEFORE more general ones.",
                            later.name,
                            later_prefix,
                            earlier.name,
                            earlier_prefixes,
                            later_prefix,
                            later.name,
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_match_wins_in_list_order() {
        let c = build_classifier(vec![CategoryDef::prefix(
            "application",
            &["com.mycompany."],
        )]);
        assert_eq!(c.classify("com.mycompany.Widget"), "application");
        assert_eq!(c.classify("java.util.HashMap"), "jdk");
        assert_eq!(c.classify("org.hibernate.Session"), "framework");
        assert_eq!(c.classify("com.other.Thing"), "other");
    }

    #[test]
    fn user_category_can_claim_subset_of_builtin() {
        // Power-user escape hatch: a user category listed first can "steal"
        // classes that would otherwise fall into a built-in category.
        let c = build_classifier(vec![CategoryDef::prefix(
            "vendored-but-ours",
            &["java.util.VendoredThing"],
        )]);
        assert_eq!(c.classify("java.util.VendoredThing"), "vendored-but-ours");
        assert_eq!(c.classify("java.util.HashMap"), "jdk");
    }

    #[test]
    fn more_specific_user_category_must_be_listed_first() {
        // "legacy" (more specific) must come before "application" (more
        // general) in the categories Vec to ever match anything.
        let c = build_classifier(vec![
            CategoryDef::prefix("legacy", &["com.mycompany.legacy."]),
            CategoryDef::prefix("application", &["com.mycompany."]),
        ]);
        assert_eq!(c.classify("com.mycompany.legacy.OldThing"), "legacy");
        assert_eq!(c.classify("com.mycompany.NewThing"), "application");
    }

    #[test]
    fn regex_matcher_works() {
        let re = regex::Regex::new(r"^com\.mycompany\.(v1|v2)\..*$").unwrap();
        let c = build_classifier(vec![CategoryDef {
            name: "versioned".to_string(),
            matchers: vec![Matcher::Regex(re)],
        }]);
        assert_eq!(c.classify("com.mycompany.v1.Thing"), "versioned");
        assert_eq!(c.classify("com.mycompany.v3.Thing"), "other");
    }

    #[test]
    fn lint_warns_on_misordered_specific_category() {
        // This test only exercises that lint_category_order doesn't panic on
        // a misordered config; the actual warning goes to stderr (no
        // in-process assertion target without capturing stderr, which this
        // repo doesn't do for other eprintln-based warnings either — see
        // collection_config.rs's precedent).
        let categories = vec![
            CategoryDef::prefix("application", &["com.mycompany."]),
            CategoryDef::prefix("legacy", &["com.mycompany.legacy."]),
        ];
        lint_category_order(&categories); // must not panic
    }
}
