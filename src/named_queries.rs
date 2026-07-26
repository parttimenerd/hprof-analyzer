//! The 20 canonical named OQL queries shared by CLI, REPL, server, and WASM.

pub struct NamedQuery {
    pub name: &'static str,
    pub display: &'static str,
    pub group: &'static str,
    pub needs_retained: bool,
    pub oql: &'static str,
}

pub const NAMED_QUERIES: &[NamedQuery] = &[
    // ── Overview ─────────────────────────────────────────────────────────────
    NamedQuery {
        name: "top-classes-by-count",
        display: "Top classes by instance count",
        group: "Overview",
        needs_retained: false,
        oql: "SELECT classof(x) AS class, COUNT(*) AS count FROM java.lang.Object x GROUP BY classof(x) ORDER BY count DESC LIMIT 30",
    },
    NamedQuery {
        name: "top-classes-by-size",
        display: "Top classes by shallow size",
        group: "Overview",
        needs_retained: false,
        oql: "SELECT classof(x) AS class, SUM(@usedHeapSize) AS bytes FROM java.lang.Object x GROUP BY classof(x) ORDER BY bytes DESC LIMIT 30",
    },
    NamedQuery {
        name: "largest-objects",
        display: "Largest individual objects",
        group: "Overview",
        needs_retained: false,
        oql: "SELECT @objectAddress, classof(x) AS class, @usedHeapSize AS bytes FROM java.lang.Object x ORDER BY bytes DESC LIMIT 20",
    },
    NamedQuery {
        name: "object-count-total",
        display: "Total object count",
        group: "Overview",
        needs_retained: false,
        oql: "SELECT COUNT(*) AS total_objects FROM java.lang.Object",
    },
    NamedQuery {
        name: "heap-summary",
        display: "Heap summary by class",
        group: "Overview",
        needs_retained: false,
        oql: "SELECT classof(x) AS class, COUNT(*) AS count, SUM(@usedHeapSize) AS total_bytes FROM java.lang.Object x GROUP BY classof(x) ORDER BY total_bytes DESC LIMIT 50",
    },
    // ── Strings ──────────────────────────────────────────────────────────────
    NamedQuery {
        name: "duplicate-strings",
        display: "Duplicate string values",
        group: "Strings",
        needs_retained: false,
        oql: "SELECT s.value AS value, COUNT(*) AS count FROM java.lang.String s GROUP BY s.value ORDER BY count DESC LIMIT 30",
    },
    NamedQuery {
        name: "largest-strings",
        display: "Largest String objects",
        group: "Strings",
        needs_retained: false,
        oql: "SELECT @objectAddress, @usedHeapSize AS bytes FROM java.lang.String ORDER BY bytes DESC LIMIT 20",
    },
    NamedQuery {
        name: "string-count",
        display: "String count and total size",
        group: "Strings",
        needs_retained: false,
        oql: "SELECT COUNT(*) AS count, SUM(@usedHeapSize) AS total_bytes FROM java.lang.String",
    },
    // ── Threads ───────────────────────────────────────────────────────────────
    NamedQuery {
        name: "all-threads",
        display: "All Thread objects",
        group: "Threads",
        needs_retained: false,
        oql: "SELECT @objectAddress, @name AS name FROM java.lang.Thread",
    },
    NamedQuery {
        name: "thread-count",
        display: "Thread count",
        group: "Threads",
        needs_retained: false,
        oql: "SELECT COUNT(*) AS count FROM java.lang.Thread",
    },
    // ── Collections ───────────────────────────────────────────────────────────
    NamedQuery {
        name: "large-arrays",
        display: "Large primitive arrays (>64 KB)",
        group: "Collections",
        needs_retained: false,
        oql: "SELECT @objectAddress, classof(x) AS class, @usedHeapSize AS bytes FROM byte[] x WHERE @usedHeapSize > 65536 UNION SELECT @objectAddress, classof(x) AS class, @usedHeapSize AS bytes FROM int[] x WHERE @usedHeapSize > 65536 UNION SELECT @objectAddress, classof(x) AS class, @usedHeapSize AS bytes FROM char[] x WHERE @usedHeapSize > 65536 ORDER BY bytes DESC LIMIT 20",
    },
    NamedQuery {
        name: "large-collections",
        display: "Large collections (>1000 elements)",
        group: "Collections",
        needs_retained: false,
        oql: "SELECT @objectAddress, classof(x) AS class, x.size AS size FROM java.util.AbstractCollection x WHERE x.size > 1000 ORDER BY size DESC LIMIT 20",
    },
    NamedQuery {
        name: "empty-collections",
        display: "Empty collections",
        group: "Collections",
        needs_retained: false,
        oql: "SELECT @objectAddress, classof(x) AS class FROM java.util.AbstractCollection x WHERE x.size = 0 LIMIT 50",
    },
    // ── Class Loaders ─────────────────────────────────────────────────────────
    NamedQuery {
        name: "class-loaders",
        display: "All ClassLoader instances",
        group: "Class Loaders",
        needs_retained: false,
        oql: "SELECT @objectAddress, classof(x) AS class FROM java.lang.ClassLoader",
    },
    NamedQuery {
        name: "classes-per-loader",
        display: "Class count per loader",
        group: "Class Loaders",
        needs_retained: false,
        oql: "SELECT classof(x) AS loader, COUNT(*) AS class_count FROM java.lang.ClassLoader x GROUP BY classof(x) ORDER BY class_count DESC",
    },
    // ── Retained ──────────────────────────────────────────────────────────────
    NamedQuery {
        name: "top-retained-by-class",
        display: "Top retained size by class",
        group: "Retained",
        needs_retained: true,
        oql: "SELECT classof(x) AS class, SUM(@retainedHeapSize) AS retained_bytes FROM java.lang.Object x GROUP BY classof(x) ORDER BY retained_bytes DESC LIMIT 30",
    },
    NamedQuery {
        name: "largest-retained-objects",
        display: "Largest retained objects",
        group: "Retained",
        needs_retained: true,
        oql: "SELECT @objectAddress, classof(x) AS class, @retainedHeapSize AS retained_bytes FROM java.lang.Object x ORDER BY retained_bytes DESC LIMIT 20",
    },
    NamedQuery {
        name: "leak-suspects",
        display: "Leak suspects (retained >10 MB)",
        group: "Retained",
        needs_retained: true,
        oql: "SELECT @objectAddress, classof(x) AS class, @retainedHeapSize AS retained_bytes FROM java.lang.Object x WHERE @retainedHeapSize > 10000000 ORDER BY retained_bytes DESC LIMIT 20",
    },
    NamedQuery {
        name: "retained-threads",
        display: "Threads by retained size",
        group: "Retained",
        needs_retained: true,
        oql: "SELECT @objectAddress, @name AS name, @retainedHeapSize AS retained_bytes FROM java.lang.Thread ORDER BY retained_bytes DESC",
    },
    NamedQuery {
        name: "retained-summary",
        display: "Shallow vs retained by class",
        group: "Retained",
        needs_retained: true,
        oql: "SELECT classof(x) AS class, SUM(@usedHeapSize) AS shallow, SUM(@retainedHeapSize) AS retained_bytes FROM java.lang.Object x GROUP BY classof(x) ORDER BY retained_bytes DESC LIMIT 30",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse::parse;

    #[test]
    fn all_20_queries_defined() {
        assert_eq!(NAMED_QUERIES.len(), 20);
    }

    #[test]
    fn all_queries_parse() {
        for nq in NAMED_QUERIES {
            parse(nq.oql).unwrap_or_else(|e| {
                panic!("named query {:?} failed to parse: {}", nq.name, e.0);
            });
        }
    }

    #[test]
    fn needs_retained_matches_attribute() {
        for nq in NAMED_QUERIES {
            let has_attr = nq.oql.contains("@retainedHeapSize");
            assert_eq!(
                nq.needs_retained, has_attr,
                "query {:?}: needs_retained={} but @retainedHeapSize present={}",
                nq.name, nq.needs_retained, has_attr
            );
        }
    }

    #[test]
    fn names_are_kebab_case_unique() {
        let mut seen = std::collections::HashSet::new();
        for nq in NAMED_QUERIES {
            assert!(
                nq.name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "query name {:?} is not kebab-case",
                nq.name
            );
            assert!(seen.insert(nq.name), "duplicate query name {:?}", nq.name);
        }
    }
}
