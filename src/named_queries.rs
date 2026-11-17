//! Named OQL queries shared by CLI, REPL, server, and WASM.

#![allow(dead_code)]

pub struct NamedQuery {
    pub name: &'static str,
    pub display: &'static str,
    pub group: &'static str,
    pub needs_retained: bool,
    /// I7: view needs collection analysis at load time (MCP
    /// `load_dump({collections:true})`, CLI `analyze --collections`); on a
    /// cache built without it the view silently returns zero rows.
    pub needs_collections: bool,
    pub oql: &'static str,
}

pub const NAMED_QUERIES: &[NamedQuery] = &[
    // ── Overview ─────────────────────────────────────────────────────────────
    NamedQuery {
        name: "top-classes-by-count",
        display: "Top classes by instance count",
        group: "Overview",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT classof(x) AS class, COUNT(*) AS count FROM INSTANCEOF java.lang.Object x GROUP BY classof(x) ORDER BY count DESC LIMIT 30",
    },
    NamedQuery {
        name: "top-classes-by-size",
        display: "Top classes by shallow size",
        group: "Overview",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT classof(x) AS class, SUM(@usedHeapSize) AS bytes FROM INSTANCEOF java.lang.Object x GROUP BY classof(x) ORDER BY bytes DESC LIMIT 30",
    },
    NamedQuery {
        name: "largest-objects",
        display: "Largest individual objects",
        group: "Overview",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, classof(x) AS class, @usedHeapSize AS bytes FROM INSTANCEOF java.lang.Object x ORDER BY bytes DESC LIMIT 20",
    },
    NamedQuery {
        name: "object-count-total",
        display: "Total object count",
        group: "Overview",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT COUNT(*) AS total_objects FROM INSTANCEOF java.lang.Object",
    },
    NamedQuery {
        name: "heap-summary",
        display: "Heap summary by class",
        group: "Overview",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT classof(x) AS class, COUNT(*) AS count, SUM(@usedHeapSize) AS total_bytes FROM INSTANCEOF java.lang.Object x GROUP BY classof(x) ORDER BY total_bytes DESC LIMIT 50",
    },
    // ── Strings ──────────────────────────────────────────────────────────────
    NamedQuery {
        name: "duplicate-strings",
        display: "Duplicate string values",
        group: "Strings",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT toString(s) AS value, COUNT(*) AS count FROM java.lang.String s GROUP BY toString(s) ORDER BY count DESC LIMIT 30",
    },
    NamedQuery {
        name: "largest-strings",
        display: "Largest String objects",
        group: "Strings",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, toString(s) AS value, @usedHeapSize AS bytes FROM java.lang.String s ORDER BY bytes DESC LIMIT 20",
    },
    NamedQuery {
        name: "string-count",
        display: "String count and total size",
        group: "Strings",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT COUNT(*) AS count, SUM(@usedHeapSize) AS total_bytes FROM java.lang.String",
    },
    // ── Threads ───────────────────────────────────────────────────────────────
    NamedQuery {
        name: "all-threads",
        display: "All Thread objects",
        group: "Threads",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, @name AS name FROM java.lang.Thread",
    },
    NamedQuery {
        name: "thread-count",
        display: "Thread count",
        group: "Threads",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT COUNT(*) AS count FROM java.lang.Thread",
    },
    NamedQuery {
        name: "thread-pool-waiters",
        display: "Blocked/waiting threads by retained size",
        group: "Threads",
        needs_retained: true,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, @name AS thread_name, @retainedHeapSize AS retained_bytes FROM java.lang.Thread ORDER BY retained_bytes DESC LIMIT 30",
    },
    // ── Collections ───────────────────────────────────────────────────────────
    NamedQuery {
        name: "large-arrays",
        display: "Large primitive arrays (>64 KB)",
        group: "Collections",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, classof(x) AS class, @usedHeapSize AS bytes FROM byte[] x WHERE @usedHeapSize > 65536 UNION SELECT @objectId AS idx, classof(x) AS class, @usedHeapSize AS bytes FROM int[] x WHERE @usedHeapSize > 65536 UNION SELECT @objectId AS idx, classof(x) AS class, @usedHeapSize AS bytes FROM char[] x WHERE @usedHeapSize > 65536 ORDER BY bytes DESC LIMIT 20",
    },
    NamedQuery {
        name: "large-collections",
        display: "Large collections (>1000 elements)",
        group: "Collections",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, classof(x) AS class, x.size() AS size FROM INSTANCEOF java.util.AbstractCollection x WHERE x.size() > 1000 ORDER BY size DESC LIMIT 20",
    },
    NamedQuery {
        name: "empty-collections",
        display: "Empty collections",
        group: "Collections",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, classof(x) AS class FROM INSTANCEOF java.util.AbstractCollection x WHERE x.size() = 0 LIMIT 50",
    },
    // ── Class Loaders ─────────────────────────────────────────────────────────
    NamedQuery {
        name: "class-loaders",
        display: "All ClassLoader instances",
        group: "Class Loaders",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, classof(x) AS class FROM INSTANCEOF java.lang.ClassLoader",
    },
    NamedQuery {
        name: "classes-per-loader",
        display: "Loaders (instance count per loader class)",
        group: "Class Loaders",
        needs_retained: false,
        needs_collections: false,
        // C5: OQL cannot express "count java.lang.Class objects per defining
        // loader". The hprof format records each class's defining loader only
        // in the CLASS_DUMP header (`pass1::ClassInfo.loader_id`), which no
        // OQL attribute exposes: the Attr enum has no loader attribute,
        // `classof()` takes a bare alias only (no field paths), and Class
        // objects are CLASS_DUMP records with no instance fields to
        // RefPath-walk. This view therefore honestly counts ClassLoader
        // INSTANCES grouped by loader class. True per-loader class counts
        // live in the report pipeline (`LoaderRollup.class_count`):
        // get_report({"section":"classloaders"}) / `heap report --section
        // classloaders`.
        oql: "SELECT classof(x) AS loader_class, COUNT(*) AS loader_instances FROM INSTANCEOF java.lang.ClassLoader x GROUP BY classof(x) ORDER BY loader_instances DESC",
    },
    // ── Retained ──────────────────────────────────────────────────────────────
    NamedQuery {
        name: "top-retained-by-class",
        display: "Top retained size by class",
        group: "Retained",
        needs_retained: true,
        needs_collections: false,
        oql: "SELECT classof(x) AS class, SUM(@retainedHeapSize) AS retained_bytes FROM INSTANCEOF java.lang.Object x GROUP BY classof(x) ORDER BY retained_bytes DESC LIMIT 30",
    },
    NamedQuery {
        name: "largest-retained-objects",
        display: "Largest retained objects",
        group: "Retained",
        needs_retained: true,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, classof(x) AS class, @retainedHeapSize AS retained_bytes FROM INSTANCEOF java.lang.Object x ORDER BY retained_bytes DESC LIMIT 20",
    },
    NamedQuery {
        name: "leak-suspects",
        display: "Leak suspects (retained >10 MB)",
        group: "Retained",
        needs_retained: true,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, classof(x) AS class, @retainedHeapSize AS retained_bytes FROM INSTANCEOF java.lang.Object x WHERE @retainedHeapSize > 10000000 ORDER BY retained_bytes DESC LIMIT 20",
    },
    NamedQuery {
        name: "retained-threads",
        display: "Threads by retained size",
        group: "Retained",
        needs_retained: true,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, @name AS name, @retainedHeapSize AS retained_bytes FROM java.lang.Thread ORDER BY retained_bytes DESC",
    },
    NamedQuery {
        name: "retained-summary",
        display: "Shallow vs retained by class",
        group: "Retained",
        needs_retained: true,
        needs_collections: false,
        oql: "SELECT classof(x) AS class, SUM(@usedHeapSize) AS shallow, SUM(@retainedHeapSize) AS retained_bytes FROM INSTANCEOF java.lang.Object x GROUP BY classof(x) ORDER BY retained_bytes DESC LIMIT 30",
    },
    // ── Exploration ───────────────────────────────────────────────────────────
    NamedQuery {
        name: "xml-dom-documents",
        display: "Accumulated DOM Document objects (Xerces)",
        group: "Exploration",
        needs_retained: true,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, @retainedHeapSize AS ret, classof(x) AS class FROM INSTANCEOF com.sun.org.apache.xerces.internal.dom.CoreDocumentImpl x ORDER BY ret DESC LIMIT 20",
    },
    NamedQuery {
        name: "fjp-task-queue",
        display: "ForkJoinTask subtypes accumulating",
        group: "Exploration",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT classof(x) AS class, COUNT(*) AS n, SUM(@usedHeapSize) AS total_shallow FROM INSTANCEOF java.util.concurrent.ForkJoinTask x GROUP BY classof(x) ORDER BY n DESC LIMIT 20",
    },
    NamedQuery {
        name: "classloader-instances",
        display: "Live ClassLoader instances with retained sizes",
        group: "Exploration",
        needs_retained: true,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, classof(x) AS class, @retainedHeapSize AS ret FROM INSTANCEOF java.lang.ClassLoader x ORDER BY ret DESC LIMIT 30",
    },
    NamedQuery {
        name: "big-decimal-instances",
        display: "BigDecimal accumulation by type",
        group: "Exploration",
        needs_retained: false,
        needs_collections: false,
        oql: "SELECT classof(x) AS class, COUNT(*) AS n, SUM(@usedHeapSize) AS total_shallow FROM INSTANCEOF java.math.BigDecimal x GROUP BY classof(x) ORDER BY n DESC LIMIT 10",
    },
    NamedQuery {
        name: "http-sessions",
        display: "HTTP Session objects by retained size",
        group: "Exploration",
        needs_retained: true,
        needs_collections: false,
        oql: "SELECT @objectId AS idx, classof(x) AS class, @retainedHeapSize AS ret FROM INSTANCEOF javax.servlet.http.HttpSession x ORDER BY ret DESC LIMIT 30",
    },
    NamedQuery {
        name: "static-collections",
        display: "Collection classes by retained size (spots high retained/shallow ratio)",
        group: "Exploration",
        needs_retained: true,
        needs_collections: true,
        oql: "SELECT classof(x) AS class, COUNT(*) AS n, SUM(@usedHeapSize) AS total_shallow, SUM(@retainedHeapSize) AS total_retained FROM INSTANCEOF java.util.Collection x GROUP BY classof(x) ORDER BY total_retained DESC LIMIT 20",
    },
    NamedQuery {
        name: "unclosed-resources",
        display: "File / stream / JDBC resource handle instance counts",
        group: "Exploration",
        needs_retained: false,
        needs_collections: true,
        oql: "SELECT classof(x) AS class, COUNT(*) AS n, SUM(@usedHeapSize) AS total_shallow FROM INSTANCEOF java.io.Closeable x GROUP BY classof(x) ORDER BY n DESC LIMIT 30",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse::parse;

    #[test]
    fn all_28_queries_defined() {
        assert_eq!(NAMED_QUERIES.len(), 28);
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

    #[test]
    fn needs_collections_flags_exactly_the_interface_views() {
        let flagged: Vec<&str> = NAMED_QUERIES
            .iter()
            .filter(|nq| nq.needs_collections)
            .map(|nq| nq.name)
            .collect();
        assert_eq!(
            flagged,
            vec!["static-collections", "unclosed-resources"],
            "needs_collections must flag exactly the INSTANCEOF-interface views"
        );
    }

    #[test]
    fn classes_per_loader_is_honest_about_what_it_counts() {
        let nq = NAMED_QUERIES
            .iter()
            .find(|nq| nq.name == "classes-per-loader")
            .expect("classes-per-loader view exists");
        // C5: the old OQL aliased the loader-instance count as "class_count"
        // under a "Class count per loader" label. The view now counts
        // ClassLoader instances per loader class and says so.
        assert!(nq.display.contains("instance count"));
        assert!(
            !nq.display.to_lowercase().contains("class count"),
            "display {:?} must not promise class counts",
            nq.display
        );
        assert!(nq.oql.contains("COUNT(*) AS loader_instances"));
        assert!(
            !nq.oql.contains("class_count"),
            "OQL must not alias the instance count as class_count: {}",
            nq.oql
        );
        assert!(
            nq.oql.contains("AS loader_class"),
            "OQL must label the group key as the loader CLASS: {}",
            nq.oql
        );
    }
}
