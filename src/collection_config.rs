// src/collection_config.rs
#![allow(dead_code)]

use crate::pass2::{CollDesc, CollKind, builtin_coll_descs};
use std::path::{Path, PathBuf};

/// Parse a `Class#field` string into `(field_name, declaring_class)`.
/// If no `#` is present, `class_context` is used as the declaring class.
pub(crate) fn parse_class_field(spec: &str, class_context: &str) -> (String, String) {
    if let Some(pos) = spec.find('#') {
        let owner = spec[..pos].to_string();
        let field = spec[pos + 1..].to_string();
        (field, owner)
    } else {
        (spec.to_string(), class_context.to_string())
    }
}

fn parse_kind(s: &str) -> Result<CollKind, String> {
    match s {
        "Map" => Ok(CollKind::Map),
        "Set" => Ok(CollKind::Set),
        "List" => Ok(CollKind::List),
        "Deque" => Ok(CollKind::Deque),
        "Queue" => Ok(CollKind::Queue),
        "Tree" => Ok(CollKind::Tree),
        other => Err(format!(
            "unknown collection kind: {other:?}; expected Map|Set|List|Deque|Queue|Tree"
        )),
    }
}

#[derive(serde::Deserialize)]
struct RawEntry {
    class: String,
    #[serde(default = "default_kind")]
    kind: String,
    size_field: Option<String>,
    array_field: Option<String>,
    nested_map_field: Option<String>,
}

fn default_kind() -> String {
    "List".into()
}

#[derive(serde::Deserialize)]
struct ConfigFile {
    #[serde(default)]
    collection: Vec<RawEntry>,
    #[serde(default)]
    query: Vec<RawQuery>,
    #[serde(default)]
    category: Vec<RawCategory>,
}

/// A `[[category]]` entry from the config file: a name plus one or more
/// `prefix`/`regex` matchers. Checked in FILE ORDER, before the two
/// built-in categories (`framework`, `jdk`) — see `classify.rs` module
/// docs for the full "first match wins" / "list specific before general"
/// semantics.
#[derive(serde::Deserialize)]
struct RawCategory {
    name: String,
    #[serde(default)]
    prefix: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
}

/// A `[[query]]` entry from the config file: an OQL string with an optional
/// display name. The `oql` may embed a leading `-- @viz` directive line, which
/// the query intake strips and honors just like a `--query` argument.
#[derive(serde::Deserialize)]
struct RawQuery {
    name: Option<String>,
    oql: String,
}

/// A named OQL query loaded from config, ready to hand to the query intake.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConfigQuery {
    pub name: Option<String>,
    pub oql: String,
}

/// Parse a TOML string into a Vec of user-defined CollDesc entries.
/// CFG-03: validates per-entry; invalid entries are warned and skipped,
/// so one bad `[[collection]]` doesn't drop all valid entries.
pub(crate) fn parse_toml_str(src: &str) -> Result<Vec<CollDesc>, String> {
    let cfg: ConfigFile = toml::from_str(src).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for e in cfg.collection {
        let class_name = e.class.replace('.', "/");
        let kind = match parse_kind(&e.kind) {
            Ok(k) => k,
            Err(msg) => {
                eprintln!("warning: skipping [[collection]] {:?}: {msg}", class_name);
                continue;
            }
        };
        let size_field = e.size_field.map(|s| {
            let (f, o) = parse_class_field(&s, &class_name);
            (f, o.replace('.', "/"))
        });
        let array_field = e.array_field.map(|s| {
            let (f, o) = parse_class_field(&s, &class_name);
            (f, o.replace('.', "/"))
        });
        let nested_map_field = e.nested_map_field.map(|s| {
            let (f, o) = parse_class_field(&s, &class_name);
            (f, o.replace('.', "/"))
        });
        out.push(CollDesc {
            class_name,
            size_field,
            array_field,
            nested_map_field,
            kind,
        });
    }
    Ok(out)
}

/// Parse the `[[query]]` entries from a TOML config string. Empty when the
/// file declares none. An entry with a blank `oql` is a hard error naming it.
pub(crate) fn parse_query_entries(src: &str) -> Result<Vec<ConfigQuery>, String> {
    let cfg: ConfigFile = toml::from_str(src).map_err(|e| e.to_string())?;
    cfg.query
        .into_iter()
        .map(|q| {
            if q.oql.trim().is_empty() {
                return Err(format!(
                    "config [[query]] entry {:?} has an empty `oql`",
                    q.name.as_deref().unwrap_or("<unnamed>")
                ));
            }
            Ok(ConfigQuery {
                name: q.name,
                oql: q.oql,
            })
        })
        .collect()
}

/// Load `[[query]]` entries from the config file (same search order as
/// [`load_collection_descs`]). A parse error is surfaced as a warning and the
/// entries are dropped, matching the collection-config failure mode.
pub(crate) fn load_config_queries(explicit_path: Option<&Path>) -> Vec<ConfigQuery> {
    find_config(explicit_path)
        .and_then(|p| {
            std::fs::read_to_string(&p)
                .map_err(|e| eprintln!("warning: could not read query config {}: {e}", p.display()))
                .ok()
        })
        .and_then(|src| {
            parse_query_entries(&src)
                .map_err(|e| eprintln!("warning: query config parse error: {e}"))
                .ok()
        })
        .unwrap_or_default()
}

/// Parse the `[[category]]` entries from a TOML config string, in file
/// order. An entry with neither `prefix` nor `regex` set is a hard error
/// naming it (a category with no matchers can never classify anything —
/// almost certainly a typo). Dotted class-name prefixes are accepted as-is
/// (categories classify against `pretty_class`, the dotted display form,
/// unlike `[[collection]]` entries which classify against the slash-form
/// internal name).
pub(crate) fn parse_category_entries(
    src: &str,
) -> Result<Vec<crate::classify::CategoryDef>, String> {
    let cfg: ConfigFile = toml::from_str(src).map_err(|e| e.to_string())?;
    cfg.category
        .into_iter()
        .map(|c| {
            if c.prefix.is_empty() && c.regex.is_empty() {
                return Err(format!(
                    "config [[category]] entry {:?} has neither `prefix` nor `regex` — \
                     it can never match any class",
                    c.name
                ));
            }
            let mut matchers: Vec<crate::classify::Matcher> = c
                .prefix
                .into_iter()
                .map(crate::classify::Matcher::Prefix)
                .collect();
            for pattern in c.regex {
                let re = regex::Regex::new(&pattern).map_err(|e| {
                    format!(
                        "config [[category]] entry {:?} has an invalid regex {:?}: {e}",
                        c.name, pattern
                    )
                })?;
                matchers.push(crate::classify::Matcher::Regex(re));
            }
            Ok(crate::classify::CategoryDef {
                name: c.name,
                matchers,
            })
        })
        .collect()
}

/// Load `[[category]]` entries from the config file (same search order as
/// [`load_collection_descs`]). A parse error is surfaced as a warning and
/// the entries are dropped, matching the collection/query config failure
/// mode — a malformed config degrades to "no user categories", not a hard
/// failure of the whole analysis.
pub(crate) fn load_config_categories(
    explicit_path: Option<&Path>,
) -> Vec<crate::classify::CategoryDef> {
    find_config(explicit_path)
        .and_then(|p| {
            std::fs::read_to_string(&p)
                .map_err(|e| {
                    eprintln!(
                        "warning: could not read category config {}: {e}",
                        p.display()
                    )
                })
                .ok()
        })
        .and_then(|src| {
            parse_category_entries(&src)
                .map_err(|e| eprintln!("warning: category config parse error: {e}"))
                .ok()
        })
        .unwrap_or_default()
}

/// Merge user entries (prepended) with built-in entries.
/// User entries come first so they shadow built-ins for the same class.
pub(crate) fn merge_descs(user: Vec<CollDesc>, builtins: Vec<CollDesc>) -> Vec<CollDesc> {
    let mut out = user;
    out.extend(builtins);
    out
}

/// Load collection descriptors: built-ins plus optional user config.
/// Config file search order (first found wins):
///   1. explicit_path if Some
///   2. .hprof-analyzer.toml in CWD
///   3. $HOME/.config/hprof-analyzer/.hprof-analyzer.toml  (unified name)
pub(crate) fn load_collection_descs(explicit_path: Option<&Path>) -> Vec<CollDesc> {
    let user = find_config(explicit_path)
        .and_then(|p| {
            std::fs::read_to_string(&p)
                .map_err(|e| {
                    eprintln!(
                        "warning: could not read collection config {}: {e}",
                        p.display()
                    )
                })
                .ok()
        })
        .and_then(|src| {
            parse_toml_str(&src)
                .map_err(|e| eprintln!("warning: collection config parse error: {e}"))
                .ok()
        })
        .unwrap_or_default();
    merge_descs(user, builtin_coll_descs())
}

fn find_config(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    let cwd_candidate = PathBuf::from(".hprof-analyzer.toml");
    if cwd_candidate.exists() {
        return Some(cwd_candidate);
    }
    if let Some(home) = std::env::var_os("HOME") {
        // CFG-01: use the same filename as in CWD so users can copy their config.
        let home_candidate =
            PathBuf::from(home).join(".config/hprof-analyzer/.hprof-analyzer.toml");
        if home_candidate.exists() {
            return Some(home_candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_field_shorthand_no_hash() {
        let (field, owner) = parse_class_field("size", "com/example/MyList");
        assert_eq!(field, "size");
        assert_eq!(owner, "com/example/MyList");
    }

    #[test]
    fn parse_field_explicit_owner() {
        let (field, owner) = parse_class_field("com/example/Base#count", "com/example/Child");
        assert_eq!(field, "count");
        assert_eq!(owner, "com/example/Base");
    }

    #[test]
    fn load_descs_user_entries_prepended() {
        let toml = r#"
[[collection]]
class = "java/util/HashMap"
kind = "List"
size_field = "mySize"
"#;
        let descs = parse_toml_str(toml).unwrap();
        let loaded = merge_descs(descs, builtin_coll_descs());
        let first = loaded
            .iter()
            .find(|d| d.class_name == "java/util/HashMap")
            .unwrap();
        assert_eq!(first.kind, CollKind::List);
    }

    #[test]
    fn user_entry_shadows_builtin() {
        let toml = r#"
[[collection]]
class = "java/util/HashMap"
kind = "List"
size_field = "java/util/HashMap#size"
"#;
        let user = parse_toml_str(toml).unwrap();
        let descs = merge_descs(user, builtin_coll_descs());
        let first_hm = descs
            .iter()
            .find(|d| d.class_name == "java/util/HashMap")
            .unwrap();
        assert_eq!(first_hm.kind, CollKind::List);
        assert_eq!(
            first_hm.size_field,
            Some(("size".into(), "java/util/HashMap".into()))
        );
    }

    #[test]
    fn unknown_kind_is_skipped() {
        let toml = r#"
[[collection]]
class = "com/example/Foo"
kind = "Bag"
"#;
        // Unknown kind emits a warning and skips the entry rather than hard-erroring,
        // so the caller doesn't crash on user-config mistakes.
        let result = parse_toml_str(toml);
        assert!(result.is_ok());
        assert!(
            result.unwrap().is_empty(),
            "unknown kind entry must be skipped"
        );
    }

    #[test]
    fn parse_query_entries_reads_named_and_unnamed() {
        let toml = r#"
[[query]]
name = "threads"
oql = "SELECT * FROM java.lang.Thread"

[[query]]
oql = "SELECT COUNT(*) FROM java.lang.String"
"#;
        let qs = parse_query_entries(toml).unwrap();
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].name.as_deref(), Some("threads"));
        assert_eq!(qs[0].oql, "SELECT * FROM java.lang.Thread");
        assert_eq!(qs[1].name, None);
        assert_eq!(qs[1].oql, "SELECT COUNT(*) FROM java.lang.String");
    }

    #[test]
    fn parse_query_entries_empty_when_none_declared() {
        let toml = r#"
[[collection]]
class = "java/util/HashMap"
"#;
        assert_eq!(parse_query_entries(toml).unwrap(), vec![]);
    }

    #[test]
    fn parse_query_entries_blank_oql_is_error() {
        let toml = r#"
[[query]]
name = "bad"
oql = "   "
"#;
        let err = parse_query_entries(toml).unwrap_err();
        assert!(err.contains("bad"), "error should name the entry: {err}");
        assert!(err.contains("empty"), "error should mention empty: {err}");
    }

    #[test]
    fn parse_query_entries_preserves_viz_directive_in_oql() {
        // The `-- @viz` directive stays in the oql string; the query intake
        // strips it later. parse_query_entries must not mangle it.
        let toml = r#"
[[query]]
name = "hist"
oql = """
-- @viz histogram label=c value=n
SELECT @clazz AS c, COUNT(*) AS n FROM java.lang.Object
"""
"#;
        let qs = parse_query_entries(toml).unwrap();
        assert_eq!(qs.len(), 1);
        assert!(qs[0].oql.contains("-- @viz histogram"));
        assert!(qs[0].oql.contains("SELECT @clazz AS c"));
    }

    #[test]
    fn collections_and_queries_coexist_in_one_file() {
        let toml = r#"
[[collection]]
class = "com/example/MyList"
kind = "List"

[[query]]
oql = "SELECT * FROM C"
"#;
        assert_eq!(parse_toml_str(toml).unwrap().len(), 1);
        assert_eq!(parse_query_entries(toml).unwrap().len(), 1);
    }

    #[test]
    fn guava_immutablelist_in_builtins() {
        let descs = builtin_coll_descs();
        let entry = descs
            .iter()
            .find(|d| d.class_name == "com/google/common/collect/ImmutableList")
            .expect("ImmutableList missing from builtins");
        assert_eq!(entry.kind, CollKind::List);
        assert_eq!(
            entry.array_field,
            Some((
                "array".into(),
                "com/google/common/collect/ImmutableList".into()
            ))
        );
    }

    #[test]
    fn new_jdk_collections_in_builtins() {
        let descs = builtin_coll_descs();
        let names: Vec<&str> = descs.iter().map(|d| d.class_name.as_str()).collect();

        // JDK collections added beyond the original set.
        for class in &[
            "java/util/LinkedHashSet",
            "java/util/WeakHashMap",
            "java/util/EnumMap",
            "java/util/PriorityQueue",
            "java/util/IdentityHashMap",
            "java/util/concurrent/CopyOnWriteArrayList",
            "java/util/concurrent/ArrayBlockingQueue",
            "java/util/concurrent/LinkedBlockingQueue",
            "java/util/concurrent/ConcurrentLinkedQueue",
            "java/util/concurrent/ConcurrentSkipListMap",
            "java/util/concurrent/ConcurrentSkipListSet",
        ] {
            assert!(names.contains(class), "{class} missing from builtins");
        }

        // Java 9+ immutable collections.
        for class in &[
            "java/util/ImmutableCollections$List12",
            "java/util/ImmutableCollections$ListN",
            "java/util/ImmutableCollections$Set12",
            "java/util/ImmutableCollections$SetN",
            "java/util/ImmutableCollections$Map1",
            "java/util/ImmutableCollections$MapN",
        ] {
            assert!(names.contains(class), "{class} missing from builtins");
        }

        // Collections wrapper classes.
        for class in &[
            "java/util/Collections$UnmodifiableList",
            "java/util/Collections$SynchronizedMap",
            "java/util/Collections$SingletonList",
            "java/util/Collections$EmptySet",
        ] {
            assert!(names.contains(class), "{class} missing from builtins");
        }

        // Verify field layouts for a few entries.
        let pq = descs
            .iter()
            .find(|d| d.class_name == "java/util/PriorityQueue")
            .unwrap();
        assert_eq!(pq.kind, CollKind::Queue);
        assert_eq!(
            pq.size_field,
            Some(("size".into(), "java/util/PriorityQueue".into()))
        );
        assert_eq!(
            pq.array_field,
            Some(("queue".into(), "java/util/PriorityQueue".into()))
        );

        let listn = descs
            .iter()
            .find(|d| d.class_name == "java/util/ImmutableCollections$ListN")
            .unwrap();
        assert_eq!(listn.kind, CollKind::List);
        assert_eq!(
            listn.array_field,
            Some((
                "elements".into(),
                "java/util/ImmutableCollections$ListN".into()
            ))
        );
    }
}
