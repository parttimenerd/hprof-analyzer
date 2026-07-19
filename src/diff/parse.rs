//! MAT report ingestion: detect the input kind (zip / directory / html),
//! extract the HTML documents, and parse the comparable tables/prose into a
//! `MatReport`. Byte-for-byte identical to the pre-split `diff.rs`.

use std::io::{self, Read};
use std::path::Path;

use super::*;
use crate::report::Report;

// ── Input detection & loading ────────────────────────────────────────────────

/// Which side of the `--diff` pair a path represents.
#[derive(Debug, PartialEq)]
pub(crate) enum Side {
    Mat,
    Json,
}

pub(crate) fn classify_side(path: &str) -> io::Result<Side> {
    let p = Path::new(path);
    if p.is_dir() {
        return Ok(Side::Mat);
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".json") {
        return Ok(Side::Json);
    }
    if lower.ends_with(".zip") || lower.ends_with(".html") || lower.ends_with(".htm") {
        return Ok(Side::Mat);
    }
    // Fall back to sniffing the file contents.
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 8];
    let n = f.read(&mut head)?;
    let head = &head[..n];
    if head.starts_with(b"PK") {
        return Ok(Side::Mat); // zip magic
    }
    let trimmed: &[u8] = head
        .iter()
        .position(|&b| !b.is_ascii_whitespace())
        .map(|i| &head[i..])
        .unwrap_or(head);
    if trimmed.first() == Some(&b'{') {
        return Ok(Side::Json);
    }
    Ok(Side::Mat)
}

/// Load our JSON report from a path.
pub(crate) fn load_json(path: &str) -> io::Result<Report> {
    let s = std::fs::read_to_string(path)?;
    serde_json::from_str(&s).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid report JSON: {e}"),
        )
    })
}

/// A named HTML document extracted from the MAT report input.
struct HtmlDoc {
    /// Lowercased file name (relative), used to detect index vs pages.
    name: String,
    html: String,
}

/// Collect the HTML documents from a MAT report input, whether it is a `.zip`,
/// an unzipped directory, or a single `.html` file.
fn load_mat_html(path: &str) -> io::Result<Vec<HtmlDoc>> {
    let p = Path::new(path);
    let lower = path.to_ascii_lowercase();
    if p.is_dir() {
        return collect_dir_html(p);
    }
    if lower.ends_with(".html") || lower.ends_with(".htm") {
        let html = std::fs::read_to_string(path)?;
        return Ok(vec![HtmlDoc {
            name: p
                .file_name()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default(),
            html,
        }]);
    }
    // Treat as a zip.
    read_zip_html(path)
}

fn collect_dir_html(dir: &Path) -> io::Result<Vec<HtmlDoc>> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<HtmlDoc>) -> io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out)?;
            } else if let Some(ext) = path.extension() {
                if ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm") {
                    if let Ok(html) = std::fs::read_to_string(&path) {
                        out.push(HtmlDoc {
                            name: path
                                .file_name()
                                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                                .unwrap_or_default(),
                            html,
                        });
                    }
                }
            }
        }
        Ok(())
    }
    walk(dir, &mut out)?;
    Ok(out)
}

/// Read all `*.html` members out of a MAT report zip.
///
/// MAT report zips are plain (stored/deflated) ZIPs; we parse the central
/// directory + local headers with `flate2` for the deflated members, avoiding
/// a heavyweight zip crate for this small use.
fn read_zip_html(path: &str) -> io::Result<Vec<HtmlDoc>> {
    let bytes = std::fs::read(path)?;
    let mut out = Vec::new();
    // Locate End-Of-Central-Directory record (scan from the end).
    let eocd = find_eocd(&bytes)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a zip: no EOCD record"))?;
    let cd_count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]) as usize;
    let cd_off = u32::from_le_bytes([
        bytes[eocd + 16],
        bytes[eocd + 17],
        bytes[eocd + 18],
        bytes[eocd + 19],
    ]) as usize;

    let mut p = cd_off;
    for _ in 0..cd_count {
        if p + 46 > bytes.len() || &bytes[p..p + 4] != b"PK\x01\x02" {
            break;
        }
        let method = u16::from_le_bytes([bytes[p + 10], bytes[p + 11]]);
        let comp_size =
            u32::from_le_bytes([bytes[p + 20], bytes[p + 21], bytes[p + 22], bytes[p + 23]])
                as usize;
        let name_len = u16::from_le_bytes([bytes[p + 28], bytes[p + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[p + 30], bytes[p + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[p + 32], bytes[p + 33]]) as usize;
        let lho = u32::from_le_bytes([bytes[p + 42], bytes[p + 43], bytes[p + 44], bytes[p + 45]])
            as usize;
        // The 46-byte fixed header is proven present by the `p + 46` guard above,
        // but `name_len` is a 16-bit field (up to 65535) read from the dump. A
        // truncated or hostile zip can declare a name extending past the buffer;
        // slicing `p+46 .. p+46+name_len` would then panic. Bound-check the full
        // variable-length record before reading it (mirrors the local-header
        // guard below), and bail out cleanly on overrun.
        if p + 46 + name_len + extra_len + comment_len > bytes.len() {
            break;
        }
        let name = String::from_utf8_lossy(&bytes[p + 46..p + 46 + name_len]).to_string();
        p += 46 + name_len + extra_len + comment_len;

        let low = name.to_ascii_lowercase();
        if !(low.ends_with(".html") || low.ends_with(".htm")) {
            continue;
        }
        // Parse the local file header to find the data offset.
        if lho + 30 > bytes.len() || &bytes[lho..lho + 4] != b"PK\x03\x04" {
            continue;
        }
        let l_name = u16::from_le_bytes([bytes[lho + 26], bytes[lho + 27]]) as usize;
        let l_extra = u16::from_le_bytes([bytes[lho + 28], bytes[lho + 29]]) as usize;
        let data_off = lho + 30 + l_name + l_extra;
        if data_off + comp_size > bytes.len() {
            continue;
        }
        let data = &bytes[data_off..data_off + comp_size];
        let html = match method {
            0 => String::from_utf8_lossy(data).to_string(), // stored
            8 => {
                use flate2::read::DeflateDecoder;
                let mut dec = DeflateDecoder::new(data);
                let mut s = String::new();
                dec.read_to_string(&mut s)?;
                s
            }
            _ => continue,
        };
        // Keep only the base name for detection (e.g. "index.html",
        // "class_histogram6.html").
        let base = low.rsplit('/').next().unwrap_or(&low).to_string();
        out.push(HtmlDoc { name: base, html });
    }
    Ok(out)
}

fn find_eocd(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 22 {
        return None;
    }
    let start = bytes.len().saturating_sub(22 + 65_536);
    (start..=bytes.len() - 22).rev().find(|&i| {
        if &bytes[i..i + 4] != b"PK\x05\x06" {
            return false;
        }
        // Validate the candidate: the EOCD's trailing comment length must be
        // consistent with the record's position, and the central-directory
        // offset/size must lie within the file. This rejects a stray
        // `PK\x05\x06` byte sequence appearing inside HTML content.
        let comment_len = u16::from_le_bytes([bytes[i + 20], bytes[i + 21]]) as usize;
        if i + 22 + comment_len != bytes.len() {
            return false;
        }
        let cd_size =
            u32::from_le_bytes([bytes[i + 12], bytes[i + 13], bytes[i + 14], bytes[i + 15]])
                as usize;
        let cd_off =
            u32::from_le_bytes([bytes[i + 16], bytes[i + 17], bytes[i + 18], bytes[i + 19]])
                as usize;
        cd_off + cd_size <= i
    })
}

// ── HTML parsing (scraper) ───────────────────────────────────────────────────

/// Strip thousands separators and parse a base-10 integer.
fn parse_int(s: &str) -> Option<u64> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if cleaned.is_empty() {
        None
    } else {
        cleaned.parse().ok()
    }
}

/// Parse the System Overview `index.html`: the `<table class="result">` of
/// LABEL/VALUE rows.
pub fn parse_system_overview(html: &str, out: &mut MatReport) {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let table_sel = Selector::parse("table.result").unwrap();
    let row_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();

    for table in doc.select(&table_sel) {
        for row in table.select(&row_sel) {
            // Skip the trailing totals row.
            if row
                .value()
                .attr("class")
                .map(|c| c.contains("totals"))
                .unwrap_or(false)
            {
                continue;
            }
            let tds: Vec<String> = row
                .select(&td_sel)
                .map(|td| td.text().collect::<String>().trim().to_string())
                .collect();
            if tds.len() != 2 {
                continue;
            }
            let (label, value) = (tds[0].as_str(), tds[1].as_str());
            match label {
                "Used heap dump" => out.used_heap_dump = Some(value.to_string()),
                "Number of objects" => out.number_of_objects = parse_int(value),
                "Number of classes" => out.number_of_classes = parse_int(value),
                "Number of class loaders" => out.number_of_class_loaders = parse_int(value),
                "Number of GC roots" => out.number_of_gc_roots = parse_int(value),
                "Format" => out.format = Some(value.to_string()),
                "File length" => out.file_length = parse_int(value),
                _ => {}
            }
        }
    }
}

/// Parse a Class_Histogram page: `<table class="result">` with data rows and a
/// trailing `<tr class="totals">` carrying exact grand totals.
pub fn parse_class_histogram(html: &str, out: &mut MatReport) {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let table_sel = Selector::parse("table.result").unwrap();
    let row_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let a_sel = Selector::parse("a[href^=\"mat://object/\"]").unwrap();

    let Some(table) = doc.select(&table_sel).next() else {
        return;
    };
    for row in table.select(&row_sel) {
        let is_totals = row
            .value()
            .attr("class")
            .map(|c| c.contains("totals"))
            .unwrap_or(false);
        let tds: Vec<_> = row.select(&td_sel).collect();
        if is_totals {
            // <td>...Total...</td><td>OBJECTS</td><td>SHALLOW</td><td></td>
            if tds.len() >= 3 {
                out.histogram_total_objects = parse_int(&tds[1].text().collect::<String>());
                out.histogram_total_shallow = parse_int(&tds[2].text().collect::<String>());
            }
            continue;
        }
        if tds.len() < 3 {
            continue; // header row has <th>, no <td>
        }
        // CLASSNAME = text of the first <a href="mat://object/..."> in first td.
        let Some(a) = tds[0].select(&a_sel).next() else {
            continue;
        };
        let class_name = a.text().collect::<String>().trim().to_string();
        let objects = parse_int(&tds[1].text().collect::<String>());
        let shallow = parse_int(&tds[2].text().collect::<String>());
        let retained = tds
            .get(3)
            .and_then(|td| parse_int(&td.text().collect::<String>()));
        if let (Some(objects), Some(shallow)) = (objects, shallow) {
            out.histogram.push(MatHistRow {
                class_name,
                objects,
                shallow,
                retained,
            });
        }
    }
}

/// Parse the Leak_Suspects `index.html`: the exact-value prose in
/// `<div class="important">`.
pub fn parse_leak_suspects(html: &str, out: &mut MatReport) {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let imp_sel = Selector::parse("div.important").unwrap();
    let q_sel = Selector::parse("q").unwrap();
    let strong_sel = Selector::parse("strong").unwrap();

    for imp in doc.select(&imp_sel) {
        let full_text = imp.text().collect::<String>();
        let trimmed = full_text.trim_start();
        // MAT's leakhunter phrases a suspect in one of three ways:
        //   (1) "N instances of <q>CLASS</q>, loaded by ... occupy BYTES"
        //   (2) "The class <q>CLASS</q>, loaded by ..., occupies BYTES"
        //   (3) "The thread <strong>THREAD @ 0xADDR  name</strong> keeps local
        //        variables with total size BYTES. The top consumers ... are
        //        <q>CONSUMER</q> ...  accumulated in one instance of
        //        <q>THREAD-CLASS</q> ..."
        // In (3) the FIRST <q> is a top-CONSUMER class, not the suspect; the
        // suspect is the thread, whose class we take from the bare <strong>
        // thread label (normalizing away the " @ 0xADDR  name" suffix). Using
        // the first <q> here would misname the suspect (regression: dump_2
        // named the suspect `cafesat.sat.Literal` instead of `java.lang.Thread`).
        let is_thread_variant = trimmed.starts_with("The thread ");
        let class_name = if is_thread_variant {
            // First bare <strong> = thread label "java.lang.Thread @ 0x..  name".
            let Some(st) = imp.select(&strong_sel).next() else {
                continue;
            };
            normalize_mat_object_label(&st.text().collect::<String>())
        } else {
            // Suspect class name: first <q>.
            let Some(q) = imp.select(&q_sel).next() else {
                continue;
            };
            q.text().collect::<String>().trim().to_string()
        };
        // "N instances of" prefix -> instance count (absent for "The class X"
        // and for the thread variant, where the thread is a single object).
        let instance_count = if is_thread_variant {
            Some(1)
        } else {
            full_text.split_whitespace().next().and_then(parse_int)
        };
        // Exact bytes + pct: the <strong> matching "NNN (PP.PP%)".
        let mut retained = None;
        let mut pct = None;
        for st in imp.select(&strong_sel) {
            let t = st.text().collect::<String>();
            if let Some((bytes, p)) = parse_bytes_pct(&t) {
                retained = Some(bytes);
                pct = Some(p);
                break;
            }
        }
        if let (Some(retained), Some(pct)) = (retained, pct) {
            out.suspects.push(MatSuspect {
                class_name,
                instance_count,
                retained,
                pct,
            });
        }
    }
}

/// Parse a `<strong>` text like "2,791,424 (22.90%)" into (bytes, pct).
fn parse_bytes_pct(t: &str) -> Option<(u64, f64)> {
    let open = t.find('(')?;
    let close = t.find('%')?;
    if close < open {
        return None;
    }
    let bytes = parse_int(&t[..open])?;
    let pct: f64 = t[open + 1..close].trim().parse().ok()?;
    Some((bytes, pct))
}

/// Parse the Top_Components `index.html`: `<h2>` headers each carrying an
/// `<a href="pages/...">COMPONENT (NN%)</a>`.
pub fn parse_top_components(html: &str, out: &mut MatReport) {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let h2_sel = Selector::parse("h2").unwrap();
    let a_sel = Selector::parse("a[href^=\"pages/\"]").unwrap();
    for h2 in doc.select(&h2_sel) {
        let Some(a) = h2.select(&a_sel).next() else {
            continue;
        };
        let txt = a.text().collect::<String>();
        let txt = txt.trim();
        // COMPONENT (NN%)
        if let Some(open) = txt.rfind('(') {
            if let Some(pctpos) = txt[open..].find('%') {
                let name = txt[..open].trim().to_string();
                let pct = parse_int(&txt[open + 1..open + pctpos]);
                if let (false, Some(pct)) = (name.is_empty(), pct) {
                    out.components.push(MatComponent {
                        name,
                        pct: pct as u32,
                    });
                }
            }
        }
    }
}

/// Normalize a MAT dominator-tree object label to its bare class name.
///
/// MAT labels an object as `[class ]<CLASS> @ 0x<ADDR>[  <thread-name>]`
/// (the leading `class ` prefix appears for java.lang.Class instances). Our
/// `ObjRow.display_class` is the bare class name, so we strip the optional
/// `class ` prefix and everything from the ` @ 0x` address marker onward.
fn normalize_mat_object_label(label: &str) -> String {
    let s = label.trim();
    let s = s.strip_prefix("class ").unwrap_or(s);
    // Cut at the address marker " @ 0x".
    let s = match s.find(" @ 0x") {
        Some(i) => &s[..i],
        None => s,
    };
    normalize_array_len(s.trim())
}

/// Normalize array-instance length annotations to the class-level array form:
/// MAT names an individual array OBJECT with its element count (e.g.
/// `InstanceBlock[7]`, `java.lang.Object[131072]`, `int[7][]`), while our
/// `display_class` uses the array TYPE name (`InstanceBlock[]`). Strip the
/// digits inside every `[...]` so the two agree; the length is a display detail,
/// not a heap-size fact (shallow/retained still compare exactly).
pub(crate) fn normalize_array_len(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_bracket = false;
    for c in s.chars() {
        match c {
            '[' => {
                in_bracket = true;
                out.push('[');
            }
            ']' => {
                in_bracket = false;
                out.push(']');
            }
            d if in_bracket && d.is_ascii_digit() => {} // drop the length digits
            _ => out.push(c),
        }
    }
    out
}

/// Parse the "Top Consumers" page (`Top_Consumers*.html`), present in both the
/// Leak_Suspects and Top_Components report zips. It carries three comparable
/// tables:
///   * "Biggest Objects" — a dominator-tree table (Class Name / Shallow /
///     Retained), one row per top dominator object.
///   * "Biggest Top-Level Dominator Classes" — Label / #Objects / Used Heap /
///     Retained Heap / Retained%.
///   * "Biggest Top-Level Dominator Packages" — a pruned package tree with an
///     ASCII tree-prefix in the first cell encoding nesting depth.
pub fn parse_top_consumers(html: &str, out: &mut MatReport) {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let table_sel = Selector::parse("table.result").unwrap();
    let row_sel = Selector::parse("tr").unwrap();
    let td_sel = Selector::parse("td").unwrap();
    let th_sel = Selector::parse("th").unwrap();
    let obj_a_sel = Selector::parse("a[href^=\"mat://object/\"]").unwrap();
    let li_sel = Selector::parse("li").unwrap();

    for table in doc.select(&table_sel) {
        // Identify the table by its header cells.
        let headers: Vec<String> = table
            .select(&th_sel)
            .map(|th| th.text().collect::<String>().trim().to_string())
            .collect();
        let is_objects = headers == ["Class Name", "Shallow Heap", "Retained Heap"];
        let is_classes = headers.first().map(|h| h == "Label").unwrap_or(false)
            && headers.iter().any(|h| h == "Number of Objects")
            && headers.iter().any(|h| h == "Retained Heap Size");
        let is_packages = headers.first().map(|h| h == "Package").unwrap_or(false);

        if is_objects {
            for row in table.select(&row_sel) {
                if row_is_totals(&row) {
                    continue;
                }
                let tds: Vec<_> = row.select(&td_sel).collect();
                if tds.len() < 3 {
                    continue;
                }
                let Some(a) = tds[0].select(&obj_a_sel).next() else {
                    continue;
                };
                let label = a.text().collect::<String>();
                let class_name = normalize_mat_object_label(&label);
                let shallow = parse_int(&tds[1].text().collect::<String>());
                let retained = parse_int(&tds[2].text().collect::<String>());
                if let (Some(shallow), Some(retained)) = (shallow, retained) {
                    out.biggest_objects.push(MatBiggestObject {
                        class_name,
                        shallow,
                        retained,
                    });
                }
            }
        } else if is_classes {
            for row in table.select(&row_sel) {
                if row_is_totals(&row) {
                    continue;
                }
                let tds: Vec<_> = row.select(&td_sel).collect();
                // Label / #Objects / Used Heap / Retained Heap / Retained%
                if tds.len() < 4 {
                    continue;
                }
                let Some(a) = tds[0].select(&obj_a_sel).next() else {
                    continue;
                };
                let class_name = a.text().collect::<String>().trim().to_string();
                // The "Biggest Top-Level Dominator Class Loaders" table shares
                // the exact same header row as the Classes table. Its rows are
                // class-LOADER labels ("<system class loader>", "X @ 0xADDR"),
                // which are not classes and have no counterpart in our
                // biggest_classes — reject them so they are not spuriously
                // compared as classes.
                if class_name == "<system class loader>" || class_name.contains(" @ 0x") {
                    continue;
                }
                let objects = parse_int(&tds[1].text().collect::<String>());
                let retained = parse_int(&tds[3].text().collect::<String>());
                if let (Some(objects), Some(retained)) = (objects, retained) {
                    out.biggest_classes.push(MatBiggestClass {
                        class_name,
                        objects,
                        retained,
                    });
                }
            }
        } else if is_packages {
            // Package tree: first cell = ASCII-tree prefix + <img> + <li>SEGMENT.
            // Depth = length of the leading prefix chars (root "<all>" = 0).
            let mut path_stack: Vec<String> = Vec::new();
            for row in table.select(&row_sel) {
                if row_is_totals(&row) {
                    continue; // subtree-summary rows have no per-node counterpart
                }
                let tds: Vec<_> = row.select(&td_sel).collect();
                // Package / Retained Heap / Retained% / # Top Dominators
                if tds.len() < 4 {
                    continue;
                }
                // Raw HTML of the first cell up to the first <img> = the prefix.
                let first_html = tds[0].inner_html();
                let prefix_len = first_html
                    .find("<img")
                    .map(|i| first_html[..i].chars().count())
                    .unwrap_or(0);
                let Some(li) = tds[0].select(&li_sel).next() else {
                    continue;
                };
                // The <li> text is "SEGMENT" followed by an anchor's text; take
                // the leading text node before any child element.
                let seg_raw = li
                    .text()
                    .next()
                    .map(|t| t.trim().to_string())
                    .unwrap_or_default();
                let segment = if seg_raw == "<all>" {
                    String::new()
                } else {
                    seg_raw.clone()
                };
                let retained = parse_int(&tds[1].text().collect::<String>());
                let top_dominators = parse_int(&tds[3].text().collect::<String>());
                let (Some(retained), Some(top_dominators)) = (retained, top_dominators) else {
                    continue;
                };
                // Maintain the dotted path from the prefix depth. A node at
                // depth d sits at stack index d-1 (the root at depth 0 has the
                // empty path); truncate to d-1, then push this segment.
                if prefix_len > 0 {
                    path_stack.truncate(prefix_len - 1);
                    path_stack.push(segment.clone());
                } else {
                    path_stack.clear();
                }
                let dotted_path = path_stack.join(".");
                out.packages.push(MatPackageRow {
                    depth: prefix_len,
                    segment,
                    dotted_path,
                    retained,
                    top_dominators,
                });
            }
        }
    }
}

/// True if this row is a MAT `class="totals"` summary row.
fn row_is_totals(row: &scraper::ElementRef) -> bool {
    row.value()
        .attr("class")
        .map(|c| c.contains("totals"))
        .unwrap_or(false)
}

/// Dispatch every HTML doc to the right parser based on its file name / content.
fn parse_mat_docs(docs: &[HtmlDoc]) -> MatReport {
    let mut rep = MatReport::default();
    // The whole-heap "Top Consumers" page (Biggest Objects/Classes/Packages of
    // the ENTIRE heap) ships as the single `Top_Consumers*.html` in the
    // Leak_Suspects and System_Overview zips. The Top_Components zip instead
    // ships SEVERAL class-loader-SCOPED Top Consumers pages (one per component),
    // whose tables are relative to a single component and have no whole-heap
    // counterpart in our `top` model. Only parse the whole-heap page: require
    // that exactly one such page is present.
    let top_consumer_docs: Vec<&HtmlDoc> = docs
        .iter()
        .filter(|d| d.name.contains("top_consumers"))
        .collect();
    let parse_whole_heap_top = top_consumer_docs.len() == 1;
    for doc in docs {
        let n = &doc.name;
        if n.contains("class_histogram") {
            parse_class_histogram(&doc.html, &mut rep);
        } else if n.contains("top_consumers") {
            if parse_whole_heap_top {
                parse_top_consumers(&doc.html, &mut rep);
            }
        } else if n == "index.html" || n == "index.htm" {
            // The index could belong to any of the three report types. Detect
            // by content and run whichever parsers find data.
            if doc.html.contains("Problem Suspect") || doc.html.contains("class=\"important\"") {
                parse_leak_suspects(&doc.html, &mut rep);
            }
            if doc.html.contains("Top Components") {
                parse_top_components(&doc.html, &mut rep);
            }
            if doc.html.contains("Used heap dump") || doc.html.contains("class=\"result\"") {
                parse_system_overview(&doc.html, &mut rep);
            }
        }
    }
    rep
}

/// Load and parse a MAT report input (zip/dir/html) into a `MatReport`.
pub fn load_mat_report(path: &str) -> io::Result<MatReport> {
    let docs = load_mat_html(path)?;
    Ok(parse_mat_docs(&docs))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A malformed central-directory record whose `name_len` field overruns the
    /// buffer must not panic `read_zip_html` — it must bail out cleanly. Before
    /// the bound check the `&bytes[p+46..p+46+name_len]` slice panicked with an
    /// index-out-of-bounds on truncated/hostile zip input.
    #[test]
    fn read_zip_html_survives_oversized_name_len() {
        // Central-directory record: 46-byte fixed header claiming name_len=1000
        // but no name bytes actually follow (record sits flush against EOCD).
        let mut cd = Vec::new();
        cd.extend_from_slice(b"PK\x01\x02"); // central dir signature
        cd.extend_from_slice(&[0u8; 6]); // version made by / needed, flags
        cd.extend_from_slice(&[0u8; 2]); // method (stored)
        cd.extend_from_slice(&[0u8; 8]); // mod time/date, crc32
        cd.extend_from_slice(&0u32.to_le_bytes()); // comp size
        cd.extend_from_slice(&0u32.to_le_bytes()); // uncomp size
        cd.extend_from_slice(&1000u16.to_le_bytes()); // name_len — LIES, overruns
        cd.extend_from_slice(&0u16.to_le_bytes()); // extra_len
        cd.extend_from_slice(&0u16.to_le_bytes()); // comment_len
        cd.extend_from_slice(&[0u8; 8]); // disk#, int/ext attrs
        cd.extend_from_slice(&0u32.to_le_bytes()); // local header offset
        assert_eq!(cd.len(), 46);

        let cd_off = 0u32;
        let cd_size = cd.len() as u32;

        // EOCD record that find_eocd will accept: 1 central-dir entry, and
        // cd_off+cd_size <= eocd position, comment_len consistent with EOF.
        let mut eocd = Vec::new();
        eocd.extend_from_slice(b"PK\x05\x06"); // EOCD signature
        eocd.extend_from_slice(&0u16.to_le_bytes()); // disk number
        eocd.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
        eocd.extend_from_slice(&1u16.to_le_bytes()); // entries this disk
        eocd.extend_from_slice(&1u16.to_le_bytes()); // total entries
        eocd.extend_from_slice(&cd_size.to_le_bytes()); // cd size
        eocd.extend_from_slice(&cd_off.to_le_bytes()); // cd offset
        eocd.extend_from_slice(&0u16.to_le_bytes()); // comment length

        let mut blob = cd;
        blob.extend_from_slice(&eocd);

        let dir = std::env::temp_dir();
        let path = dir.join(format!("hprof_zip_fuzz_{}.zip", std::process::id()));
        std::fs::write(&path, &blob).unwrap();

        // Must return (Ok or Err), never panic.
        let result = read_zip_html(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
        assert!(
            result.is_ok(),
            "read_zip_html should bail cleanly, not error, on an overrun name_len"
        );
        assert!(
            result.unwrap().is_empty(),
            "no HTML docs should be extracted from the malformed record"
        );
    }
}
