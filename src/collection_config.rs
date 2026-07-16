// src/collection_config.rs

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
}

/// Parse a TOML string into a Vec of user-defined CollDesc entries.
pub(crate) fn parse_toml_str(src: &str) -> Result<Vec<CollDesc>, String> {
    let cfg: ConfigFile = toml::from_str(src).map_err(|e| e.to_string())?;
    cfg.collection
        .into_iter()
        .map(|e| {
            let kind = parse_kind(&e.kind)?;
            let size_field = e.size_field.map(|s| {
                let (f, o) = parse_class_field(&s, &e.class);
                (f, o)
            });
            let array_field = e.array_field.map(|s| {
                let (f, o) = parse_class_field(&s, &e.class);
                (f, o)
            });
            let nested_map_field = e.nested_map_field.map(|s| {
                let (f, o) = parse_class_field(&s, &e.class);
                (f, o)
            });
            Ok(CollDesc {
                class_name: e.class,
                size_field,
                array_field,
                nested_map_field,
                kind,
            })
        })
        .collect()
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
///   3. $HOME/.config/hprof-analyzer/collections.toml
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
        let home_candidate = PathBuf::from(home).join(".config/hprof-analyzer/collections.toml");
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
    fn unknown_kind_is_error() {
        let toml = r#"
[[collection]]
class = "com/example/Foo"
kind = "Bag"
"#;
        assert!(parse_toml_str(toml).is_err());
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
}
