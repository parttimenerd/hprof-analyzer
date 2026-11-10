//! Live `ClassResolver` over pass2's in-memory class metadata, plus a driver
//! that fans each per-object callback out to the active SingleScan executors.
//! Built and driven inside `Pass2::build` during the 2a heap scan.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::id_map::IdMap;
use crate::pass1::ClassInfo;
use crate::query::ObjectVisitor;
use crate::query::ast::Query;
use crate::query::execute::{ClassResolver, SingleScanExecutor, canonical_distinct_key_pub};
use crate::query::model::{QueryResult, QueryValue};
use crate::query::plan::QueryPlan;
use crate::types::HprofType;

/// Resolves a class-object address (`class_id`) to its dotted class name and,
/// for named fields, to the `(offset, type)` within an INSTANCE_DUMP blob. Also
/// serves per-object `@objectAddress` (via `id_map`) and `@usedHeapSize` (via
/// the dense `shallow` size array). Borrows pass2's live tables immutably for
/// the scan's lifetime.
#[allow(clippy::type_complexity)]
pub struct LiveResolver<'a> {
    class_map: &'a HashMap<u64, ClassInfo>,
    strings: &'a HashMap<u64, String>,
    id_size: usize,
    names: HashMap<u64, String>,
    id_map: &'a IdMap,
    shallow: &'a [u32],
    field_cache: RefCell<HashMap<(u64, String), Option<(u32, HprofType)>>>,
}

impl<'a> LiveResolver<'a> {
    pub fn new(
        class_map: &'a HashMap<u64, ClassInfo>,
        strings: &'a HashMap<u64, String>,
        id_size: usize,
        id_map: &'a IdMap,
        shallow: &'a [u32],
    ) -> Self {
        let mut names = HashMap::with_capacity(class_map.len());
        for (&addr, ci) in class_map {
            if let Some(raw) = strings.get(&ci.name_id) {
                names.insert(addr, raw.replace('/', "."));
            }
        }
        Self {
            class_map,
            strings,
            id_size,
            names,
            id_map,
            shallow,
            field_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Walk the super-chain from `class_id`, returning the SLASH-form name of the
    /// first class that declares a field named `name` (so `field_offset`'s
    /// `owner_class` filter selects the right declaring class, not a shadowing
    /// subclass field of the same simple name).
    fn owner_of(&self, class_id: u64, name: &str) -> Option<String> {
        let mut cur = class_id;
        loop {
            let ci = self.class_map.get(&cur)?;
            for &(fname_id, _t) in &ci.fields {
                if self.strings.get(&fname_id).map(String::as_str) == Some(name) {
                    return self.strings.get(&ci.name_id).cloned();
                }
            }
            if ci.super_id == 0 {
                return None;
            }
            cur = ci.super_id;
        }
    }
}

impl<'a> ClassResolver for LiveResolver<'a> {
    fn class_name(&self, class_id: u64) -> Option<&str> {
        self.names.get(&class_id).map(String::as_str)
    }

    /// `FROM INSTANCEOF C` / `WHERE x INSTANCEOF C`: match the object's class AND
    /// every superclass. Walk the `super_id` chain from `class_id` (mirroring
    /// `owner_of`), testing each class's dot-form name against `spec`. Returns
    /// true on the first match; false once the chain terminates (`super_id == 0`)
    /// or a link is missing. `spec.instanceof` is ignored here — this method is
    /// only reached WHEN instanceof is requested, and it always walks the chain.
    fn is_instance_of(
        &self,
        class_id: u64,
        spec: &crate::query::ast::ClassSpec,
        from_regex: Option<&regex::Regex>,
    ) -> bool {
        let mut cur = class_id;
        loop {
            if let Some(name) = self.names.get(&cur) {
                if crate::query::execute::class_name_matches_spec(name, spec, from_regex) {
                    return true;
                }
            }
            match self.class_map.get(&cur) {
                Some(ci) if ci.super_id != 0 => cur = ci.super_id,
                _ => return false,
            }
        }
    }

    fn field(&self, class_id: u64, name: &str) -> Option<(u32, HprofType)> {
        let key = (class_id, name.to_string());
        if let Some(cached) = self.field_cache.borrow().get(&key) {
            return *cached;
        }
        let resolved = self.owner_of(class_id, name).and_then(|owner_slash| {
            crate::pass2::sizing::field_offset(
                class_id,
                name,
                &owner_slash,
                self.class_map,
                self.strings,
                self.id_size,
            )
        });
        self.field_cache.borrow_mut().insert(key, resolved);
        resolved
    }

    fn addr_of(&self, src_idx: usize) -> Option<u64> {
        (src_idx < self.id_map.len()).then(|| self.id_map.addr_at(src_idx))
    }

    fn shallow_of(&self, src_idx: usize) -> Option<u32> {
        self.shallow.get(src_idx).copied()
    }

    fn index_of_addr(&self, addr: u64) -> Option<usize> {
        self.id_map.index_of(addr)
    }

    fn ref_width(&self) -> usize {
        self.id_size
    }
}

impl<'a> crate::query::plan::FieldSchema for LiveResolver<'a> {
    fn class_field_names(&self, exact_class_name: &str) -> Option<Vec<String>> {
        // `names` is dot-form; the FROM class as written may be dot- or
        // slash-form, so normalize both to dots before matching.
        let want = exact_class_name.replace('/', ".");
        let (&class_id, _) = self.names.iter().find(|(_, n)| **n == want)?;

        let mut fields = Vec::new();
        let mut cur = class_id;
        while let Some(ci) = self.class_map.get(&cur) {
            for &(fname_id, _t) in &ci.fields {
                if let Some(name) = self.strings.get(&fname_id) {
                    if !fields.iter().any(|f| f == name) {
                        fields.push(name.clone());
                    }
                }
            }
            if ci.super_id == 0 {
                break;
            }
            cur = ci.super_id;
        }
        Some(fields)
    }
}

/// Fans each `visit_instance` out to every active SingleScan executor. Each
/// executor is tagged with its `slot` (the original index in the caller's query
/// list) so `finish_state` can reassemble results in input order and route
/// cross-phase (carry) executors to the late stage.
pub struct ScanDriver<'q, R: ClassResolver> {
    execs: Vec<SingleScanExecutor<'q, R>>,
    slots: Vec<usize>,
    /// Armed only when at least one exec's plan has `needs.ref_walk`. Holds the
    /// interned hop-field table + the resolver used to decode ref fields from
    /// each instance blob. `None` on non-RefWalk runs → zero capture cost.
    refwalk: Option<RefWalkState<'q, R>>,
    /// Armed only when at least one exec's plan has `needs.string_values`.
    /// Captures `dense_idx → (arr_addr, coder)` during the scan; resolved to
    /// `String` values post-scan via a single `scan_prim_arrays` pass.
    /// `None` on non-toString runs → zero capture cost.
    string_capture: Option<StringCaptureState<'q, R>>,
    /// When true, each row-mode executor is armed to capture its per-row source
    /// dense index (for GC-reachability pruning). Set ONLY on `--reachable-only`
    /// runs; false everywhere else so the sidecar is never allocated and behavior
    /// stays byte/RSS-identical.
    capture_src: bool,
}

/// Sidecar edge-capture state for RefWalk queries (see `refwalk.rs`).
struct RefWalkState<'q, R: ClassResolver> {
    edges: crate::query::refwalk::RefWalkEdges,
    /// Interned hop field names; `field_id` is the index into this table.
    field_names: Vec<String>,
    /// Tail (projected) field names captured per resolved-target object.
    tail_names: Vec<String>,
    /// `dense_idx -> tail field value`, decoded at scan time (blob is gone in
    /// the late window). Keyed by the object that OWNS the tail field.
    tails: crate::query::refwalk::RefWalkTails,
    /// Armed when a RefPath tail is `@length` (e.g. `s.value.@length`). The late
    /// window can't derive an array's element count from its dense index alone,
    /// so each visited array's length is captured into `tails` keyed by the
    /// array's own dense index. Left `false` (no capture) otherwise so
    /// non-Length runs stay byte/RSS-identical.
    needs_length_tail: bool,
    /// Armed when a RefPath tail is `@objectAddress` (e.g. `e.getKey()` lowered to
    /// `RefPath{hops:["key"], tail:ObjectAddress}`). The dense→address table is
    /// compressed away before the late window (`IdMap::new(&[])`), so each visited
    /// object's OWN address is captured into `tails` keyed by its own dense index;
    /// the walk resolves to that index and the late window reads the address back.
    /// Left `false` (no capture) otherwise so non-address runs stay
    /// byte/RSS-identical.
    needs_address_tail: bool,
    /// Armed when any FROM ENTRIES query is present. The expansion in the late
    /// window needs array→element edges in the refwalk CSR to iterate collection
    /// backing arrays; without them expand_list_array returns no rows. Triggers
    /// visit_arrays so pass2 calls visit_object_array_elements.
    needs_array_elements: bool,
    resolver: &'q R,
}

/// Sidecar capture state for toString(s) queries. Armed only when at least one
/// query has `needs.string_values`. Captures (dense_idx → (arr_addr, coder))
/// during the scan so a post-scan array decode pass can resolve the text values.
struct StringCaptureState<'q, R: ClassResolver> {
    capture: crate::query::stringvals::StringCapture,
    /// Memoized per-class String field offsets: class_id → Option<(value_off, coder_off)>.
    /// `None` means "not a String class or field not found".
    off_cache: HashMap<u64, Option<(usize, Option<usize>)>>,
    resolver: &'q R,
}

impl<'q, R: ClassResolver> ScanDriver<'q, R> {
    /// Construct a driver from `(slot, executor)` pairs. `slot` is the query's
    /// index in the caller's list. Arms RefWalk edge capture iff any executor's
    /// plan requests it. Arms string-values capture iff any executor's plan
    /// requests toString(s).
    pub fn new(entries: Vec<(usize, SingleScanExecutor<'q, R>)>) -> Self {
        let mut execs = Vec::with_capacity(entries.len());
        let mut slots = Vec::with_capacity(entries.len());
        for (slot, ex) in entries {
            slots.push(slot);
            execs.push(ex);
        }
        let refwalk = Self::arm_refwalk(&execs);
        let string_capture = Self::arm_string_capture(&execs);
        Self {
            execs,
            slots,
            refwalk,
            string_capture,
            capture_src: false,
        }
    }

    /// Enable per-row source-index capture (for `--reachable-only` pruning).
    /// Chainable after `new`. Arms each executor's sidecar NOW — before the scan
    /// runs — so the per-row source index is captured during `visit_instance` /
    /// `visit_array` (arming only takes effect on row-mode executors; carry /
    /// aggregate executors ignore it). When `capture` is false (the default),
    /// nothing is armed and every run stays byte/RSS-identical.
    pub fn with_src_capture(mut self, capture: bool) -> Self {
        self.capture_src = capture;
        if capture {
            for ex in &mut self.execs {
                ex.arm_row_capture();
            }
        }
        self
    }

    /// True when every executor has reached its `scan_limit` and no further
    /// visits can produce new rows. When `true`, the outer scan loop can break
    /// early rather than visiting every remaining object. Safe to call after
    /// each `visit_instance`/`visit_array` call.
    pub fn is_done(&self) -> bool {
        !self.execs.is_empty() && self.execs.iter().all(|e| e.scan_satisfied())
    }

    /// Build the string-capture sidecar if any exec has `needs.string_values`.
    /// Returns `None` when no toString(s) query is present.
    fn arm_string_capture(
        execs: &[SingleScanExecutor<'q, R>],
    ) -> Option<StringCaptureState<'q, R>> {
        let needs_any = execs.iter().any(|e| e.plan().needs.string_values);
        if !needs_any {
            return None;
        }
        let resolver = execs.first().map(|e| e.resolver())?;
        Some(StringCaptureState {
            capture: crate::query::stringvals::StringCapture::new(
                crate::query::stringvals::STRING_VALUES_CAP,
            ),
            off_cache: HashMap::new(),
            resolver,
        })
    }

    /// Build the RefWalk sidecar if any exec needs it: intern the union of hop
    /// field names across all RefWalk queries, and grab a resolver reference for
    /// blob decoding. Returns `None` (no capture) when no query walks references.
    fn arm_refwalk(execs: &[SingleScanExecutor<'q, R>]) -> Option<RefWalkState<'q, R>> {
        let mut per_query_hops: Vec<Vec<String>> = Vec::new();
        let mut per_query_tails: Vec<Vec<String>> = Vec::new();
        let mut needs_length_tail = false;
        let mut needs_address_tail = false;
        let mut needs_array_elements = false;
        for ex in execs {
            if ex.plan().needs.ref_walk {
                per_query_hops.push(crate::query::refwalk::refwalk_field_names(ex.query()));
                per_query_tails.push(crate::query::refwalk::refwalk_tail_field_names(ex.query()));
                needs_length_tail |= crate::query::refwalk::refwalk_has_length_tail(ex.query());
                needs_address_tail |= crate::query::refwalk::refwalk_has_address_tail(ex.query());
                if let crate::query::ast::FromSource::Entries { class } = &ex.query().from {
                    needs_array_elements = true;
                    // The late EntriesExpand phase looks up backing-array fields
                    // (e.g. "elementData") via fwd_edge_by_name, which requires
                    // those names to be interned in the refwalk CSR's field_names
                    // table. Collect them from the built-in collection descriptors
                    // so capture_refwalk records those edges during the instance scan.
                    let slash = class.replace('.', "/");
                    let coll_descs = crate::collections::builtin_coll_descs();
                    if let Some(di) = crate::collections::find_desc_by_name(&slash, &coll_descs) {
                        let mut extra: Vec<String> = Vec::new();
                        if let Some((ref f, _)) = coll_descs[di].array_field {
                            if !extra.iter().any(|x| x == f) {
                                extra.push(f.clone());
                            }
                        }
                        if let Some((ref f, _)) = coll_descs[di].nested_map_field {
                            if !extra.iter().any(|x| x == f) {
                                extra.push(f.clone());
                            }
                        }
                        if !extra.is_empty() {
                            per_query_hops.push(extra);
                        }
                    }
                }
            }
        }
        if per_query_hops.is_empty() && !needs_array_elements {
            return None;
        }
        let field_names = crate::query::refwalk::intern_hop_fields(&per_query_hops);
        let tail_names = crate::query::refwalk::intern_hop_fields(&per_query_tails);
        let resolver = execs.first().map(|e| e.resolver())?;
        Some(RefWalkState {
            edges: crate::query::refwalk::RefWalkEdges::new(
                crate::query::refwalk::REFWALK_EDGE_CAP,
            ),
            field_names,
            tail_names,
            tails: crate::query::refwalk::RefWalkTails::new(
                crate::query::refwalk::REFWALK_EDGE_CAP,
            ),
            needs_length_tail,
            needs_address_tail,
            needs_array_elements,
            resolver,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.execs.is_empty()
    }
    /// True if any armed executor's FROM pattern can match an array class, OR a
    /// RefWalk query needs an `@length` tail (e.g. `s.value.@length`) or an
    /// `@objectAddress` tail that may land on an array (e.g. a `getValue()` hop).
    /// The latter two walk from a non-array FROM to an array target only observable
    /// via `visit_array`, so the scan must deliver arrays even though no executor's
    /// FROM is an array class. Lets the pass2 scan skip per-array class-name
    /// construction entirely when no query targets arrays (the common case),
    /// keeping the multi-GB array path allocation-free for instance-only query
    /// sets.
    pub fn wants_arrays(&self) -> bool {
        self.execs.iter().any(|e| e.wants_arrays())
            || self.refwalk.as_ref().is_some_and(|s| {
                s.needs_length_tail || s.needs_address_tail || s.needs_array_elements
            })
    }
    /// Finalize every executor into a `QueryExecState`, each tagged with its
    /// original `slot` (the query's index in the caller's list): row-mode
    /// executors push a finished `QueryResult`; carry-mode (cross-phase)
    /// executors push their carried indices as a pending entry for the late
    /// stage. Name/OQL labels are filled by the caller once results are
    /// reassembled in slot order, so they are left empty here.
    pub fn finish_state(self) -> crate::query::execute::QueryExecState {
        let mut state = crate::query::execute::QueryExecState::new();
        let slots = self.slots;
        let capture_src = self.capture_src;
        for (i, ex) in self.execs.into_iter().enumerate() {
            let slot = slots[i];
            if ex.is_carry() {
                let plan = ex.plan().clone();
                let carry = ex.take_carry();
                state.push_cross_phase(slot, String::new(), plan, carry);
            } else if capture_src {
                // Row-mode executor on a `--reachable-only` run: it was armed
                // before the scan (see `with_src_capture`), so take the captured
                // per-row source dense-index sidecar alongside the result so the
                // caller can prune unreachable rows by EXACT source index.
                let (r, src) = ex.finish_with_src("");
                state.push_finished_with_src(slot, r, src);
            } else {
                let r = ex.finish("");
                state.push_finished(slot, r);
            }
        }
        state
    }

    /// True if RefWalk edge OR tail capture overflowed its cap (owning query
    /// results must be marked truncated — the per-field CSR / tail table is
    /// incomplete so N-hop projections may miss rows).
    pub fn refwalk_truncated(&self) -> bool {
        self.refwalk
            .as_ref()
            .map(|s| s.edges.truncated() || s.tails.truncated())
            .unwrap_or(false)
    }

    /// Fold the captured field-labeled edges into a per-src forward CSR over `n`
    /// nodes: `(fwd_off[n+1], fwd_tgt, fwd_field)`. Returns `None` when RefWalk
    /// was never armed (non-RefWalk run → the late window keeps empty slices).
    /// Takes `&mut self` so it can run before `finish_state` consumes the driver.
    pub fn take_refwalk_csr(&mut self, n: usize) -> Option<(Vec<u32>, Vec<u32>, Vec<u32>)> {
        let edges = std::mem::replace(
            &mut self.refwalk.as_mut()?.edges,
            crate::query::refwalk::RefWalkEdges::new(0),
        );
        Some(edges.into_csr(n))
    }

    /// Take the captured tail-value side table (`dense_idx -> QueryValue`), or
    /// `None` when RefWalk was never armed. Takes `&mut self` for the same
    /// ordering reason as `take_refwalk_csr`.
    pub fn take_refwalk_tails(
        &mut self,
    ) -> Option<std::collections::HashMap<u32, crate::query::model::QueryValue>> {
        let tails = std::mem::replace(
            &mut self.refwalk.as_mut()?.tails,
            crate::query::refwalk::RefWalkTails::new(0),
        );
        Some(tails.into_map())
    }

    /// Take the interned hop field-name table (`field_id` → name), parallel to
    /// the CSR's `fwd_field` column. `None` when RefWalk was never armed. Takes
    /// `&mut self` for the same ordering reason as `take_refwalk_csr`.
    pub fn take_refwalk_field_names(&mut self) -> Option<Vec<String>> {
        Some(std::mem::take(&mut self.refwalk.as_mut()?.field_names))
    }

    /// True when the string-values capture overflowed its cap during the scan,
    /// meaning some String instances were not captured and results may be partial.
    pub fn string_capture_truncated(&self) -> bool {
        self.string_capture
            .as_ref()
            .map(|s| s.capture.truncated)
            .unwrap_or(false)
    }

    /// Take the string-capture side table for post-scan decoding.
    /// `None` when toString(s) was never armed (non-toString run).
    pub fn take_string_capture(&mut self) -> Option<crate::query::stringvals::StringCapture> {
        let state = self.string_capture.as_mut()?;
        Some(std::mem::replace(
            &mut state.capture,
            crate::query::stringvals::StringCapture::new(0),
        ))
    }

    /// Decode the needed reference fields from one instance blob and record their
    /// edges; also capture any tail field this object owns. No-op when unarmed.
    fn capture_refwalk(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
        let Some(state) = self.refwalk.as_mut() else {
            return;
        };
        let width = state.resolver.ref_width();
        for (field_id, name) in state.field_names.iter().enumerate() {
            let Some((off, ty)) = state.resolver.field(class_id, name) else {
                continue;
            };
            if ty != HprofType::Object {
                continue;
            }
            let start = off as usize;
            let end = start + width;
            if end > blob.len() {
                continue;
            }
            let mut addr: u64 = 0;
            for &b in &blob[start..end] {
                addr = (addr << 8) | b as u64;
            }
            if addr == 0 {
                continue; // null reference → no edge
            }
            if let Some(dst) = state.resolver.index_of_addr(addr) {
                state
                    .edges
                    .push(src_idx as u32, field_id as u32, dst as u32);
            }
        }
        // Capture tail field values owned by THIS object (keyed by its own dense
        // index — the walk resolves to it, then the late window looks it up).
        for name in &state.tail_names {
            let Some((off, ty)) = state.resolver.field(class_id, name) else {
                continue;
            };
            if let Some(v) = crate::query::refwalk::decode_primitive_tail(off, ty, blob) {
                state.tails.insert(src_idx as u32, v);
            }
        }
        // Capture THIS object's own address into the tail table, keyed by its own
        // dense index, when an `@objectAddress` RefPath tail is armed (e.g.
        // `e.getKey()`). The dense→address table is gone in the late window, so the
        // walked-to target's address is read back from here. Instances captured
        // here; arrays captured in `capture_refwalk_array_addr`.
        if state.needs_address_tail {
            if let Some(addr) = state.resolver.addr_of(src_idx) {
                state.tails.insert(
                    src_idx as u32,
                    crate::query::model::QueryValue::Int(addr as i64),
                );
            }
        }
    }

    /// Record a visited array's own address into the tail table, keyed by its own
    /// dense index, when an `@objectAddress` RefPath tail is armed (e.g. a
    /// `getValue()` hop that lands on an array). Mirrors the instance capture in
    /// `capture_refwalk`. No-op when unarmed — keeping non-address runs
    /// byte/RSS-identical.
    fn capture_refwalk_array_addr(&mut self, src_idx: usize) {
        let Some(state) = self.refwalk.as_mut() else {
            return;
        };
        if !state.needs_address_tail {
            return;
        }
        if let Some(addr) = state.resolver.addr_of(src_idx) {
            state.tails.insert(
                src_idx as u32,
                crate::query::model::QueryValue::Int(addr as i64),
            );
        }
    }

    /// Record a visited array's element count into the tail table, keyed by the
    /// array's own dense index, when a query has an `@length` RefPath tail (e.g.
    /// `s.value.@length`). The late window resolves the hop to this array's dense
    /// index, then joins it here to project the length. No-op when unarmed or no
    /// Length tail is needed — keeping non-Length runs byte/RSS-identical.
    fn capture_refwalk_array_length(&mut self, src_idx: usize, length: u32) {
        let Some(state) = self.refwalk.as_mut() else {
            return;
        };
        if !state.needs_length_tail {
            return;
        }
        state.tails.insert(
            src_idx as u32,
            crate::query::model::QueryValue::Int(length as i64),
        );
    }

    /// Capture a String instance's backing array address and coder byte for the
    /// post-scan toString decode pass. No-op when not armed or this class is not
    /// a java.lang.String class (memoized per class_id for zero amortised cost on
    /// non-String instances, which are the vast majority in any dump).
    fn capture_string_values(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
        let Some(state) = self.string_capture.as_mut() else {
            return;
        };
        // Memoize the (value_off, coder_off) for this class_id. `None` means
        // "not a java.lang.String class" — we skip it cheaply on future visits.
        let offs = *state.off_cache.entry(class_id).or_insert_with(|| {
            let value_result = state.resolver.field(class_id, "value");
            let value_off = match value_result {
                Some((off, HprofType::Object)) => off as usize,
                _ => return None,
            };
            // `coder` is a byte field (HprofType::Byte). Present in JDK9+; absent
            // in JDK8 (char[] backing — treat as UTF-16, coder = 1).
            let coder_off = state
                .resolver
                .field(class_id, "coder")
                .filter(|&(_, ty)| ty == HprofType::Byte)
                .map(|(off, _)| off as usize);
            Some((value_off, coder_off))
        });
        let Some((value_off, coder_off)) = offs else {
            return;
        };
        let width = state.resolver.ref_width();
        if value_off + width > blob.len() {
            return;
        }
        // Read the backing array reference.
        let mut arr_addr: u64 = 0;
        for &b in &blob[value_off..value_off + width] {
            arr_addr = (arr_addr << 8) | b as u64;
        }
        if arr_addr == 0 {
            return;
        } // null backing array — skip
        // Read coder byte (0=LATIN1, 1=UTF16). Default to 1 (JDK8 char[]).
        let coder = match coder_off {
            Some(co) if co < blob.len() => blob[co],
            _ => 1,
        };
        state.capture.insert(src_idx as u32, arr_addr, coder);
    }
}

impl<'q, R: ClassResolver> ObjectVisitor for ScanDriver<'q, R> {
    fn visit_instance(&mut self, src_idx: usize, class_id: u64, blob: &[u8]) {
        self.capture_refwalk(src_idx, class_id, blob);
        self.capture_string_values(src_idx, class_id, blob);
        for ex in &mut self.execs {
            if !ex.scan_satisfied() {
                ex.visit_instance(src_idx, class_id, blob);
            }
        }
    }
    fn visit_array(&mut self, src_idx: usize, class_name: &str, length: u32) {
        self.capture_refwalk_array_length(src_idx, length);
        self.capture_refwalk_array_addr(src_idx);
        for ex in &mut self.execs {
            if !ex.scan_satisfied() {
                ex.visit_array(src_idx, class_name, length);
            }
        }
    }
    fn visit_object_array_elements(&mut self, src_idx: usize, elements: &[u32]) {
        let Some(state) = self.refwalk.as_mut() else {
            return;
        };
        if !state.needs_array_elements {
            return;
        }
        // Field id 0 is the sentinel for array element slots. expand_list_array
        // iterates ALL outgoing edges of the array node without filtering by
        // field id, so any field id works here; 0 matches the test fixture.
        for &dst in elements {
            state.edges.push(src_idx as u32, 0, dst);
        }
    }
}

/// Resume a `QueryExecState` with a late context that provides the decoded
/// toString(s) string values AND (when built) the RefWalk CSR captured during
/// the query scan. Entries are routed by what late data they need:
///   * string-only (all ops `ResolveStringValues`) → resolved with the string ctx;
///   * refwalk (`plan.needs.ref_walk`, i.e. N-hop `x.field.tail`) → resolved with
///     the populated refwalk ctx via the same `run_entry_pub` path the full
///     report uses (RefWalkResolve → `refpath_rows`);
///   * everything else (retained sizes, dominators, edges) → still routed to
///     `resume_without_late_ctx` for the actionable "needs the full pipeline"
///     error, since the query-only path never builds those structures.
///
/// The refwalk ctx is gated on the CSR being present: when `refwalk_csr` is
/// `None` (no RefWalk query ran) the borrowed slices are empty and the shared
/// empty tail map is used, so non-RefWalk runs stay byte/RSS-identical.
///
/// `dfn` carries GC-reachability for `--reachable-only`: when `Some`, each
/// row-mode result is pruned to reachable rows using the EXACT per-row source
/// dense index captured during the scan (`state.row_src_by_slot`), BEFORE the
/// results leave this function and are UNION-collapsed. This is where each flat
/// result still maps 1:1 to a slot, so the source-index sidecar can be applied
/// without any UNION-collapse bookkeeping. `None` (the `--all` / default-off
/// path) prunes nothing and is byte-identical to before.
/// Flatten a pass1 IdMap into a dense `Vec<u64>` for carry-mode `@objectAddress` lookup.
/// Call this before moving `p1` into `Pass2::build` when any query uses `toString(s)`.
pub fn id_map_to_addrs(m: &IdMap) -> Vec<u64> {
    (0..m.len()).map(|i| m.addr_at(i)).collect()
}

pub(crate) fn resume_with_string_values(
    mut state: crate::query::execute::QueryExecState,
    flat: &[(Query, QueryPlan)],
    string_values: std::collections::HashMap<u32, String>,
    refwalk_csr: Option<crate::query::refwalk::RefWalkCsr>,
    dfn: Option<&[u32]>,
    // Optional: supply the real address table (pre-built via id_map_to_addrs) and
    // shallow-size array so that @objectAddress and @usedHeapSize are projected
    // correctly for carry-mode (toString) entries. Pass empty slice/None when not needed.
    addr_of: &[u64],
    shallow_opt: Option<&[u32]>,
    // Optional: dense-object-index → class-histogram row, and class name strings.
    // Pass empty slices when not available. When populated, GROUP BY @displayName
    // and related class-name queries work in the query-only fast path.
    class_idx: &[u32],
    class_names: &[String],
) -> Vec<crate::query::model::QueryResult> {
    use crate::query::plan::StageOp;
    use crate::query::stage_runner::{self, EMPTY_REFWALK_TAILS, EMPTY_STRING_VALUES};

    // Take the per-slot source-index sidecar out before the state is consumed by
    // `into_parts`. Empty (allocates nothing) unless reachability capture was
    // armed during the scan.
    let row_src_by_slot = state.take_row_src_by_slot();

    // Cross-phase (carry) pruning: a carry result's rows are projected from the
    // carried dense indices, so prune the SEEDS before the late stage runs. The
    // finished-row filter below cannot cover them: the late stage may drop
    // (deferred WHERE), reorder (ORDER BY) or cap (LIMIT) rows, which would break
    // any positional alignment. Row-expanding late ops are left untouched.
    if let Some(dfn) = dfn {
        state.prune_cross_phase_reachable(dfn);
    }

    let empty_id_map = stage_runner::IdMap::new(&[]);
    let real_id_map;
    let id_map: &stage_runner::IdMap<'_> = if addr_of.is_empty() {
        &empty_id_map
    } else {
        real_id_map = stage_runner::IdMap::new(addr_of);
        &real_id_map
    };
    let sv_ref: &std::collections::HashMap<u32, String> = if string_values.is_empty() {
        &EMPTY_STRING_VALUES
    } else {
        &string_values
    };
    // Build the refwalk ctx fields from the query-scan CSR, mirroring the full
    // report window (main.rs). Empty/None-CSR → empty slices → identical to before.
    let rw_off: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_off);
    let rw_tgt: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_tgt);
    let rw_field: &[u32] = refwalk_csr.as_ref().map_or(&[], |c| &c.fwd_field);
    let rw_names: &[String] = refwalk_csr.as_ref().map_or(&[], |c| &c.field_names);
    let rw_tails = refwalk_csr
        .as_ref()
        .map_or(&*EMPTY_REFWALK_TAILS, |c| &c.tails);
    let rw_trunc = refwalk_csr.as_ref().is_some_and(|c| c.truncated);
    let ctx = stage_runner::LateCtx {
        retained: &[],
        idom: &[],
        dc_off: &[],
        dc_tgt: &[],
        shallow: shallow_opt.unwrap_or(&[]),
        id_map,
        fwd_off: rw_off,
        fwd_tgt: rw_tgt,
        fwd_field: rw_field,
        field_names: rw_names,
        refwalk_tails: rw_tails,
        refwalk_truncated: rw_trunc,
        in_off: &[],
        in_tgt: &[],
        retained_edges: None,
        string_values: sv_ref,
        string_values_truncated: false,
        // Query-only path never collects GC roots (`@GCRoots`/`@GCRootInfo`/
        // `@info` entries route to `resume_without_late_ctx` for an actionable
        // error); the empty map keeps this path byte-identical.
        gc_root_tags: &stage_runner::EMPTY_GC_ROOT_TAGS,
        class_idx,
        class_names,
        class_categories: &[],
        class_super_idx: &[],
        coll_size_per_dense: &[],
        coll_descs: &[],
        dump_path: None,
        offsets: &[],
        id_size: 8,
    };

    // Split pending entries into four routes.
    // - toString-only: resolved with the string_values map above.
    // - refwalk: driven by the query AST via refpath_rows.
    // - array-index: resolved with the ctx above.
    // - GroupBy @displayName / instance_string: needs class_idx/class_names, handled via
    //   run_entry_pub when those arrays are populated (i.e. when the caller passes them).
    // Everything else goes through `resume_without_late_ctx` which produces actionable
    // errors — the query-only path never builds retained sizes, dominators, or edges.
    let (finished, pending) = state.into_parts();
    let mut slotted: Vec<(usize, crate::query::model::QueryResult)> = finished;
    let mut other_state = crate::query::execute::QueryExecState::new();
    // Track the slots pushed into other_state in insertion order so we can
    // re-associate them with the sorted output from resume_without_late_ctx.
    let mut other_slots: Vec<usize> = Vec::new();

    for entry in pending {
        let is_string_only = !entry.plan.late_ops.is_empty()
            && entry
                .plan
                .late_ops
                .iter()
                .all(|op| matches!(op, StageOp::ResolveStringValues));
        // A refwalk (N-hop reference-path) entry is driven entirely off the
        // query AST via `refpath_rows`; the populated refwalk ctx above is all it
        // needs. `run_entry_pub` dispatches its RefWalkResolve op to that path,
        // exactly as `stage_runner::resume` does in the full report. But route it
        // here ONLY when refwalk is the entry's SOLE late need: the query-only path
        // builds no retained/dominator/edge structures, so an entry that ALSO needs
        // those must fall to the error path — otherwise its retained/dominator/edge
        // columns would silently project Null instead of erroring actionably.
        let needs_only_refwalk =
            entry.plan.needs.ref_walk
                && !entry.plan.needs.retained
                && !entry.plan.needs.dominator_children
                && !entry.plan.late_ops.iter().any(|op| {
                    matches!(op, StageOp::EdgeLookup { .. } | StageOp::BoundedPath { .. })
                });
        // An array-index/slice entry needs P2 resolution (returning Null for all
        // ArrayIndex/ArraySlice columns in this release). Route through run_entry_pub
        // when array_index is the entry's sole late need (no retained/dominator/edge).
        let needs_only_array_index =
            entry.plan.needs.array_index
                && !entry.plan.needs.ref_walk
                && !entry.plan.needs.retained
                && !entry.plan.needs.dominator_children
                && !entry.plan.late_ops.iter().any(|op| {
                    matches!(op, StageOp::EdgeLookup { .. } | StageOp::BoundedPath { .. })
                });
        let is_refwalk = needs_only_refwalk;
        let is_array_index = needs_only_array_index;
        // GROUP BY @displayName / @classOf queries: `finalize_at = P2`, `late_ops` is
        // empty (the plan comment says "No extra late op"), `kind = GroupBy`, and
        // `needs.instance_string = true`. They call `join_retained_group_by` via
        // `run_entry_pub` → `join_retained` dispatch, which resolves class names from
        // `ctx.class_idx` / `ctx.class_names`. Route them here only when those arrays
        // are populated (caller passes them); otherwise fall through to the error path.
        let is_group_by_instance_string =
            entry.plan.kind == crate::query::plan::StageKind::GroupBy
                && entry.plan.needs.instance_string
                && !entry.plan.needs.retained
                && !entry.plan.needs.dominator_children
                && !entry.plan.needs.gc_roots
                && !entry.plan.late_ops.iter().any(|op| {
                    matches!(op, StageOp::EdgeLookup { .. } | StageOp::BoundedPath { .. })
                })
                && !class_idx.is_empty();
        if is_string_only || is_refwalk || is_array_index || is_group_by_instance_string {
            let q = &flat[entry.slot].0;
            let r = stage_runner::run_entry_pub(&entry, q, &ctx);
            slotted.push((entry.slot, r));
        } else {
            other_slots.push(entry.slot);
            other_state.push_cross_phase_entry(entry);
        }
    }

    // Delegate non-toString pending entries to the error-producing path.
    // `resume_without_late_ctx` sorts its output by slot ascending, so sort
    // `other_slots` to match and zip the two parallel sequences.
    if other_state.has_pending() {
        other_slots.sort_unstable();
        let error_results = crate::query::stage_runner::resume_without_late_ctx(other_state);
        // error_results and other_slots are now both sorted by slot ascending.
        debug_assert_eq!(other_slots.len(), error_results.len());
        for (slot, r) in other_slots.into_iter().zip(error_results) {
            slotted.push((slot, r));
        }
    }

    slotted.sort_by_key(|(slot, _)| *slot);
    // Reachable-only prune (query-subcommand default): drop each row-mode
    // result's rows whose captured SOURCE dense index is not GC-reachable. Done
    // here, per-slot and 1:1 with the sidecar, BEFORE UNION-collapse — so no
    // collapse bookkeeping is needed and `@objectAddress` rows (whose projected
    // value is an address, not an index) prune correctly. A slot with no captured
    // src (aggregate/scalar/error/refwalk) keeps all its rows.
    //
    // For pure-aggregate slots (no GROUP BY, all SELECTs are aggregates), the
    // scan captured every visited object's dense index into `row_src` so we can
    // recompute the aggregate over only the reachable subset post-DFN. Aggregates
    // produce a single summary row (not one row per object), so `filter_result_by_src`
    // would incorrectly try to keep/drop that one row — we call
    // `recompute_agg_from_reachable` instead.
    if let Some(dfn) = dfn {
        for (slot, r) in slotted.iter_mut() {
            if let Some(src) = row_src_by_slot.get(slot) {
                let is_pure_agg = flat.get(*slot).is_some_and(|(q, _)| {
                    q.group_by.is_empty()
                        && !q.select.is_empty()
                        && q.select
                            .iter()
                            .all(|it| matches!(it, crate::query::ast::SelectItem::Aggregate { .. }))
                });
                if is_pure_agg {
                    if let Some(shallow) = shallow_opt {
                        if let Some((q, _)) = flat.get(*slot) {
                            let pruned: Vec<u32> = src
                                .iter()
                                .copied()
                                .filter(|&idx| {
                                    dfn.get(idx as usize).is_some_and(|&d| d != u32::MAX)
                                })
                                .collect();
                            if let Some(new_r) = crate::query::execute::recompute_agg_from_reachable(
                                r, q, &pruned, shallow,
                            ) {
                                *r = new_r;
                            }
                        }
                    }
                } else {
                    filter_result_by_src(r, src, dfn);
                    // Re-apply LIMIT after DFN prune: the scan skipped the early-stop
                    // (because some of the N scanned rows might have been unreachable
                    // and pruned here), so the result may still exceed plan.limit.
                    // FROM-subquery queries defer their LIMIT to after the semi-join
                    // in run_single_dump (SW-6); skip re-capping them here so the
                    // semi-join sees the full reachable outer row set.
                    let is_subquery_outer = flat
                        .get(*slot)
                        .map(|(q, _)| q.from.as_subquery().is_some())
                        .unwrap_or(false);
                    if !is_subquery_outer {
                        if let Some(limit) = flat.get(*slot).and_then(|(_, p)| p.limit) {
                            let lim = limit as usize;
                            if r.rows.len() > lim {
                                r.rows.truncate(lim);
                                r.row_count = lim as u64;
                                // OQL-4: LIMIT 0 is an explicit "give me nothing"
                                // request, not a data cap — don't mark truncated.
                                if lim > 0 {
                                    r.truncated = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    slotted.into_iter().map(|(_, r)| r).collect()
}

/// Run the full pass1+pass2 pipeline against `path` for the given planned
/// queries and return their results. Used by the REPL (and available to any
/// one-shot caller). Does not build or render the full report.
///
/// Execution architecture (subqueries): the outer scan needs each
/// `IN (<subquery>)` membership set to be known DURING the scan (the predicate
/// is evaluated per object), so a single pass cannot serve it. When any query
/// uses a subquery we run a TWO-PASS scan over the same dump: an inner pass runs
/// every inner subquery as its own slot and materializes results; we then build
/// the IN-membership sets and FROM-subquery dense-index sets, inject the IN sets
/// into the outer executors, run the outer pass, and finally semi-join each
/// FROM-subquery's outer rows against its inner dense-index set. Queries without
/// subqueries take the ordinary single-pass path (no inner scan).
/// REPL-only: serve resident-only queries from a warm `ReplCache` WITHOUT a heap
/// re-scan. Builds a `LiveResolver` over the cached pass1 tables and drives each
/// query's `SingleScanExecutor` over dense indices `0..n` with an EMPTY blob
/// (safe: resident-only queries read only the resolver, never the blob).
/// Reachability pruning + UNION collapse mirror `run_single_dump` exactly.
///
/// v1 limitation: any query whose FROM targets an ARRAY class is NOT served here
/// (array class names are only recoverable per-array by address during a real
/// scan); the REPL router keeps those on the scan path. Only true instances
/// (`kind == 0`) are driven through `visit_instance` with an EMPTY blob, exactly
/// mirroring the scan path, which delivers ONLY INSTANCE_DUMP records to
/// `visit_instance` (CLASS_DUMP class objects and array records are NOT — see
/// pass2's 2a loop). Class objects (`kind == 3`), object arrays (`kind == 1`),
/// and primitive arrays (`kind == 2`) are therefore skipped, so the row set
/// matches the scan path bit-for-bit.
pub fn run_resident_only(
    cache: &ReplCache,
    queries: &[(Query, QueryPlan)],
    reachable_only: bool,
) -> std::io::Result<Vec<QueryResult>> {
    let (flat, groups) = expand_union_queries(queries);
    let resolver = LiveResolver::new(
        &cache.p1.class_map,
        &cache.p1.strings,
        cache.id_size,
        &cache.p1.id_map,
        &cache.shallow,
    );
    // Build executors: row-mode for most queries, carry-mode for toString(s)
    // queries. The toString path expects indices in `pending` (cross-phase) so
    // `resume_with_string_values` can look up each dense index in the string_values
    // map; row-mode would emit all-Null rows at scan time since blobs are empty.
    let mut entries: Vec<(usize, SingleScanExecutor<'_, LiveResolver<'_>>)> = Vec::new();
    for (slot, (q, plan)) in flat.iter().enumerate() {
        let ex = if plan.needs.string_values || plan.finalize_at != crate::query::plan::Phase::P1 {
            use crate::query::carry::Carry;
            SingleScanExecutor::new_carry(
                q,
                plan,
                &resolver,
                Carry::index_only(crate::query::carry::DEFAULT_CARRY_CAP),
            )
        } else {
            SingleScanExecutor::new(q, plan, &resolver)
        };
        entries.push((slot, ex));
    }
    let mut driver = ScanDriver::new(entries);
    // run_resident_only filters unreachable objects PRE-SCAN via dfn_for_scan
    // (below), so row_src capture is unnecessary: all collected rows are already
    // reachable, and the post-scan DFN prune in resume_with_string_values is a
    // no-op. Skip with_src_capture to preserve the early-stop optimization.

    // Drive over cached dense indices with an EMPTY blob. ONLY true instances
    // (kind 0) go through visit_instance — this mirrors the scan path exactly,
    // which sends only INSTANCE_DUMP records to visit_instance. Class objects
    // (kind 3) and arrays (kind 1/2) are skipped: array-FROM queries are routed
    // to the scan path by the caller, and the scan never delivers class objects
    // to visit_instance either.
    // When reachable_only is set, the pre-computed DFN is available here. Skip
    // unreachable objects at scan time so aggregate executors (which never
    // capture per-row source indices) also respect reachability — the post-scan
    // filter only works for row-mode queries.
    let dfn_for_scan: Option<&[u32]> = if reachable_only {
        cache.dfn.as_deref()
    } else {
        None
    };
    for i in 0..cache.n {
        if cache.p1.kind[i] == 0 {
            if dfn_for_scan.is_some_and(|d| d.get(i).copied().unwrap_or(u32::MAX) == u32::MAX) {
                continue; // unreachable — skip for all executors, including aggregates
            }
            let class_addr = cache.p1.class_addr_table[cache.class_ids[i] as usize];
            driver.visit_instance(i, class_addr, &[]);
            if driver.is_done() {
                break;
            }
        }
    }

    let state = driver.finish_state();
    let dfn: Option<&[u32]> = if reachable_only {
        cache.dfn.as_deref()
    } else {
        None
    };
    // If any query uses toString(s) the resident path needs real String values —
    // blobs are empty here so we re-scan the source once on demand.
    let needs_sv = flat.iter().any(|(_, p)| p.needs.string_values);
    let sv = if needs_sv {
        cache.build_string_values().unwrap_or_default()
    } else {
        std::collections::HashMap::new()
    };
    let addr_vec = if needs_sv
        || flat
            .iter()
            .any(|(_, p)| p.finalize_at != crate::query::plan::Phase::P1)
    {
        id_map_to_addrs(&cache.p1.id_map)
    } else {
        Vec::new()
    };
    let flat_results = resume_with_string_values(
        state,
        &flat,
        sv,
        None,
        dfn,
        &addr_vec,
        Some(&cache.shallow),
        &cache.class_idx,
        &cache.class_names,
    );
    let mut flat_results = flat_results;
    // jsonify(...) columns carry an ObjRef placeholder out of the scan; expand
    // them here, while the ReplCache (dump path + offsets.bin + pass-1 tables)
    // is still in hand and BEFORE UNION-collapse so each column can be matched
    // to its SelectItem by position rather than by its rendered name.
    crate::query::jsonify::expand_results(
        &mut flat_results,
        &flat,
        Some(cache as &dyn crate::query::jsonify::JsonifySession),
    );
    // Apply JOIN clauses and re-project to the query's SELECT list.
    // Virtual-table sources need a Graph (not available in the resident-only path);
    // VALUES-source JOINs work with graph=None.
    let flat_results: Vec<_> = flat_results
        .into_iter()
        .enumerate()
        .map(|(slot, result)| {
            if let Some((q, plan)) = flat.get(slot) {
                if !plan.join_plans.is_empty() {
                    return apply_joins_and_project(result, q, &plan.join_plans, None);
                }
            }
            result
        })
        .collect();
    Ok(collapse_union_results(flat_results, &groups))
}

/// Like `run_resident_only` but with retained-size data available from a
/// previously-run full analysis pipeline. Queries using `@retainedHeapSize`
/// (plan.needs.retained) are served from `retained` instead of re-running
/// the full dominator+retained pipeline from disk.
pub fn run_resident_with_retained(
    cache: &ReplCache,
    queries: &[(Query, QueryPlan)],
    reachable_only: bool,
    retained: &[u64],
) -> std::io::Result<Vec<QueryResult>> {
    let (flat, groups) = expand_union_queries(queries);
    let resolver = LiveResolver::new(
        &cache.p1.class_map,
        &cache.p1.strings,
        cache.id_size,
        &cache.p1.id_map,
        &cache.shallow,
    );
    let mut entries: Vec<(usize, SingleScanExecutor<'_, LiveResolver<'_>>)> = Vec::new();
    for (slot, (q, plan)) in flat.iter().enumerate() {
        let ex = if plan.needs.string_values
            || plan.needs.retained
            || plan.finalize_at != crate::query::plan::Phase::P1
        {
            use crate::query::carry::Carry;
            SingleScanExecutor::new_carry(
                q,
                plan,
                &resolver,
                Carry::index_only(crate::query::carry::DEFAULT_CARRY_CAP),
            )
        } else {
            SingleScanExecutor::new(q, plan, &resolver)
        };
        entries.push((slot, ex));
    }
    let mut driver = ScanDriver::new(entries);
    // Same pre-scan DFN filter as run_resident_only: skip unreachable objects
    // before they reach the executor, so early-stop and LIMIT are correct.
    let dfn_for_scan: Option<&[u32]> = if reachable_only {
        cache.dfn.as_deref()
    } else {
        None
    };
    for i in 0..cache.n {
        if cache.p1.kind[i] == 0 {
            if dfn_for_scan.is_some_and(|d| d.get(i).copied().unwrap_or(u32::MAX) == u32::MAX) {
                continue;
            }
            let class_addr = cache.p1.class_addr_table[cache.class_ids[i] as usize];
            driver.visit_instance(i, class_addr, &[]);
            if driver.is_done() {
                break;
            }
        }
    }
    let state = driver.finish_state();
    let dfn: Option<&[u32]> = if reachable_only {
        cache.dfn.as_deref()
    } else {
        None
    };
    // toString(s) needs real String values — blobs are empty in resident path.
    let needs_sv = flat.iter().any(|(_, p)| p.needs.string_values);
    let sv = if needs_sv {
        cache.build_string_values().unwrap_or_default()
    } else {
        std::collections::HashMap::new()
    };
    let addr_vec = if needs_sv
        || flat
            .iter()
            .any(|(_, p)| p.finalize_at != crate::query::plan::Phase::P1)
    {
        id_map_to_addrs(&cache.p1.id_map)
    } else {
        Vec::new()
    };
    let flat_results = resume_with_retained(
        state,
        &flat,
        retained,
        &cache.shallow,
        dfn,
        sv,
        &addr_vec,
        &cache.class_idx,
        &cache.class_names,
        Some(std::path::PathBuf::from(cache.source.file_path())),
        &cache.offsets,
        cache.id_size,
    );
    let mut flat_results = flat_results;
    // Same jsonify post-pass as `run_resident_only` — see the note there.
    crate::query::jsonify::expand_results(
        &mut flat_results,
        &flat,
        Some(cache as &dyn crate::query::jsonify::JsonifySession),
    );
    // Apply JOIN clauses and re-project to the query's SELECT list.
    // (Virtual-table sources need a Graph; not available here.)
    let flat_results: Vec<_> = flat_results
        .into_iter()
        .enumerate()
        .map(|(slot, result)| {
            if let Some((q, plan)) = flat.get(slot) {
                if !plan.join_plans.is_empty() {
                    return apply_joins_and_project(result, q, &plan.join_plans, None);
                }
            }
            result
        })
        .collect();
    Ok(collapse_union_results(flat_results, &groups))
}

/// Like `resume_with_string_values` but with per-object retained sizes available.
/// Routes entries that ONLY need retained sizes (no dominators, edges) through a
/// `LateCtx` that has the retained array populated. All other pending entries
/// (needing dominators/edges) fall to `resume_without_late_ctx` for actionable
/// error messages.
fn resume_with_retained(
    mut state: crate::query::execute::QueryExecState,
    flat: &[(Query, QueryPlan)],
    retained: &[u64],
    shallow: &[u32],
    dfn: Option<&[u32]>,
    string_values: std::collections::HashMap<u32, String>,
    addr_of: &[u64],
    class_idx: &[u32],
    class_names: &[String],
    dump_path: Option<std::path::PathBuf>,
    offsets: &[u64],
    id_size: usize,
) -> Vec<crate::query::model::QueryResult> {
    use crate::query::plan::StageOp;
    use crate::query::stage_runner::{
        self, EMPTY_GC_ROOT_TAGS, EMPTY_REFWALK_TAILS, IdMap, LateCtx,
    };

    let row_src_by_slot = state.take_row_src_by_slot();

    // Cross-phase (carry) pruning — see `resume_with_string_values` for why the
    // seed set, not the finished rows, is what gets filtered.
    if let Some(dfn) = dfn {
        state.prune_cross_phase_reachable(dfn);
    }

    let (finished, pending) = state.into_parts();
    let empty_id_map = IdMap::new(&[]);
    let real_id_map;
    let id_map: &IdMap<'_> = if addr_of.is_empty() {
        &empty_id_map
    } else {
        real_id_map = IdMap::new(addr_of);
        &real_id_map
    };
    let ctx = LateCtx {
        retained,
        idom: &[],
        dc_off: &[],
        dc_tgt: &[],
        shallow,
        id_map,
        fwd_off: &[],
        fwd_tgt: &[],
        fwd_field: &[],
        field_names: &[],
        refwalk_tails: &EMPTY_REFWALK_TAILS,
        refwalk_truncated: false,
        in_off: &[],
        in_tgt: &[],
        retained_edges: None,
        string_values: &string_values,
        string_values_truncated: false,
        gc_root_tags: &EMPTY_GC_ROOT_TAGS,
        class_idx,
        class_names,
        class_categories: &[],
        class_super_idx: &[],
        coll_size_per_dense: &[],
        coll_descs: &[],
        dump_path,
        offsets,
        id_size,
    };

    let mut slotted: Vec<(usize, QueryResult)> = finished;
    let mut other_state = crate::query::execute::QueryExecState::new();
    let mut other_slots: Vec<usize> = Vec::new();

    for entry in pending {
        let is_retained_only =
            entry.plan.needs.retained
                && !entry.plan.needs.dominator_children
                && !entry.plan.needs.ref_walk
                && !entry.plan.late_ops.iter().any(|op| {
                    matches!(op, StageOp::EdgeLookup { .. } | StageOp::BoundedPath { .. })
                });
        let is_string_only = !entry.plan.late_ops.is_empty()
            && entry
                .plan
                .late_ops
                .iter()
                .all(|op| matches!(op, StageOp::ResolveStringValues));
        if is_retained_only || is_string_only {
            let q = &flat[entry.slot].0;
            let r = stage_runner::run_entry_pub(&entry, q, &ctx);
            slotted.push((entry.slot, r));
        } else {
            other_slots.push(entry.slot);
            other_state.push_cross_phase_entry(entry);
        }
    }

    if other_state.has_pending() {
        other_slots.sort_unstable();
        let error_results = crate::query::stage_runner::resume_without_late_ctx(other_state);
        debug_assert_eq!(other_slots.len(), error_results.len());
        for (slot, r) in other_slots.into_iter().zip(error_results) {
            slotted.push((slot, r));
        }
    }

    slotted.sort_by_key(|(slot, _)| *slot);

    if let Some(dfn) = dfn {
        for (slot, r) in slotted.iter_mut() {
            if let Some(src) = row_src_by_slot.get(slot) {
                filter_result_by_src(r, src, dfn);
            }
        }
    }
    slotted.into_iter().map(|(_, r)| r).collect()
}

/// Execute a `FROM (VALUES (...), ...) AS alias(col1, col2, ...)` query entirely
/// in-memory, without touching the HPROF file. Applies ORDER BY and LIMIT
/// directly to the materialized rows.
pub fn execute_values_query(q: &Query, plan: &QueryPlan) -> QueryResult {
    use crate::query::ast::{AggFunc, FromSource, SelectItem, Value};
    use crate::query::execute::{
        canonical_distinct_key_pub as canonical_key, resolve_order_by_keys, sort_rows_by_columns,
    };
    use crate::query::model::QueryColumn;
    use std::collections::HashSet;

    let (col_names, value_rows) = match &q.from {
        FromSource::Values { columns, rows, .. } => (columns, rows),
        _ => {
            return QueryResult {
                name: String::new(),
                oql: String::new(),
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
                truncated: false,
                error: Some("execute_values_query called on non-VALUES query".into()),
                note: None,
                viz: None,
                elapsed_ms: None,
            };
        }
    };

    // Map Value → QueryValue.
    let value_to_qv = |v: &Value| -> QueryValue {
        match v {
            Value::Int(n) => QueryValue::Int(*n),
            Value::Float(f) => QueryValue::Float(*f),
            Value::Str(s) => QueryValue::Str(s.clone()),
            Value::Bool(b) => QueryValue::Bool(*b),
            Value::Null => QueryValue::Null,
        }
    };

    // Convert VALUES rows to QueryValue rows.
    let qv_rows: Vec<Vec<QueryValue>> = value_rows
        .iter()
        .map(|r| r.iter().map(&value_to_qv).collect())
        .collect();

    // Apply WHERE filtering.
    // Pre-materialize IN subquery sets so each row filter doesn't re-execute them.
    let filtered: Vec<&Vec<QueryValue>> = if let Some(pred) = &q.where_ {
        let mut in_sets: Vec<std::collections::HashSet<String>> = Vec::new();
        materialize_in_subquery_sets(pred, &mut in_sets);
        if in_sets.is_empty() {
            qv_rows
                .iter()
                .filter(|row| eval_pred_on_row(pred, col_names, row))
                .collect()
        } else {
            qv_rows
                .iter()
                .filter(|row| {
                    let mut cursor = 0usize;
                    eval_pred_on_row_with_sets(pred, col_names, row, &in_sets, &mut cursor)
                })
                .collect()
        }
    } else {
        qv_rows.iter().collect()
    };

    // Determine if this is an aggregate query.
    let is_aggregate = q
        .select
        .iter()
        .any(|s| matches!(s, SelectItem::Aggregate { .. }));

    let is_star = q.select.len() == 1 && matches!(q.select[0], crate::query::ast::SelectItem::Star);

    // Determine output columns.  For SELECT *, expand to all source columns; for
    // an explicit SELECT list use query_columns() so naming is consistent with all
    // other code paths (in particular: single-hop table-qualified refs like `v.label`
    // strip the table prefix and become just `label`).
    let out_cols: Vec<QueryColumn> = if is_star {
        col_names
            .iter()
            .map(|c| QueryColumn { name: c.clone() })
            .collect()
    } else {
        use crate::query::execute::query_columns;
        query_columns(q)
    };

    // Project a single row according to the SELECT list (non-aggregate path).
    // `available_cols` is the slice of column names available in `row` at projection time.
    let project_row_with = |row: &[QueryValue], available_cols: &[String]| -> Vec<QueryValue> {
        if is_star {
            row.to_vec()
        } else {
            q.select
                .iter()
                .map(|item| match item {
                    SelectItem::Attr(crate::query::ast::Attr::Field(f)) => {
                        // OQL-61: qualified refs like `t.x` may not match when the
                        // join has flattened columns to bare names. Strip the table
                        // alias prefix and retry (mirrors eval_expr_on_row logic).
                        let bare = f.rsplit_once('.').map(|(_, col)| col).unwrap_or(f.as_str());
                        let pos = available_cols.iter().position(|c| c == f).or_else(|| {
                            if bare != f {
                                available_cols.iter().position(|c| c == bare)
                            } else {
                                None
                            }
                        });
                        pos.and_then(|p| row.get(p))
                            .cloned()
                            .unwrap_or(QueryValue::Null)
                    }
                    // OQL-61: `t.x` normalizes to RefPath{hops:["t"], tail:Field("x")} when
                    // the FROM alias is not stripped (VALUES queries have q.alias=None).
                    // A single-hop RefPath in a VALUES context is just a table-qualified
                    // column ref; look up the tail field name in the available columns.
                    SelectItem::Attr(crate::query::ast::Attr::RefPath {
                        hops,
                        tail,
                        role: crate::query::ast::RefRole::ProjectionOnly,
                    }) if hops.len() == 1 => {
                        if let crate::query::ast::Attr::Field(col) = tail.as_ref() {
                            available_cols
                                .iter()
                                .position(|c| c == col)
                                .and_then(|p| row.get(p))
                                .cloned()
                                .unwrap_or(QueryValue::Null)
                        } else {
                            QueryValue::Null
                        }
                    }
                    SelectItem::Expr(expr) => eval_expr_on_row(expr, available_cols, row),
                    SelectItem::Star => row.first().cloned().unwrap_or(QueryValue::Null),
                    _ => QueryValue::Null,
                })
                .collect()
        }
    };

    // Extract the aggregate argument value from a row.
    let agg_arg_value = |arg: &SelectItem, row: &Vec<QueryValue>| -> QueryValue {
        match arg {
            SelectItem::Star => QueryValue::Int(1),
            SelectItem::Attr(crate::query::ast::Attr::Field(f)) => {
                if let Some(pos) = col_names.iter().position(|c| c == f) {
                    row.get(pos).cloned().unwrap_or(QueryValue::Null)
                } else {
                    QueryValue::Null
                }
            }
            SelectItem::Expr(expr) => eval_expr_on_row(expr, col_names, row),
            _ => QueryValue::Null,
        }
    };

    let rows: Vec<Vec<QueryValue>> = if is_aggregate {
        let agg_row: Vec<QueryValue> = q
            .select
            .iter()
            .map(|item| match item {
                SelectItem::Aggregate { func, arg, filter } => {
                    // Apply per-aggregate FILTER if present.
                    let vals: Vec<QueryValue> = filtered
                        .iter()
                        .filter(|row| {
                            filter
                                .as_ref()
                                .map(|p| eval_pred_on_row(p, col_names, row))
                                .unwrap_or(true)
                        })
                        .map(|row| agg_arg_value(arg, row))
                        .filter(|v| !matches!(v, QueryValue::Null))
                        .collect();
                    match func {
                        AggFunc::Count => QueryValue::Int(vals.len() as i64),
                        AggFunc::CountDistinct => {
                            let mut seen: HashSet<String> = HashSet::new();
                            let n = vals
                                .iter()
                                .filter(|v| seen.insert(canonical_key(v)))
                                .count();
                            QueryValue::Int(n as i64)
                        }
                        AggFunc::Sum => {
                            // E-04: preserve Int type when all operands are Int.
                            let all_int = vals.iter().all(|v| matches!(v, QueryValue::Int(_)));
                            if all_int {
                                let sum: i64 = vals
                                    .iter()
                                    .filter_map(|v| {
                                        if let QueryValue::Int(n) = v {
                                            Some(*n)
                                        } else {
                                            None
                                        }
                                    })
                                    .fold(0i64, i64::saturating_add);
                                QueryValue::Int(sum)
                            } else {
                                let sum = vals.iter().fold(0.0f64, |acc, v| match v {
                                    QueryValue::Int(n) => acc + *n as f64,
                                    QueryValue::Float(f) => acc + f,
                                    _ => acc,
                                });
                                QueryValue::Float(sum)
                            }
                        }
                        AggFunc::Min => vals
                            .iter()
                            .min_by(|a, b| {
                                use std::cmp::Ordering;
                                let af = match a {
                                    QueryValue::Int(n) => *n as f64,
                                    QueryValue::Float(f) => *f,
                                    _ => f64::NAN,
                                };
                                let bf = match b {
                                    QueryValue::Int(n) => *n as f64,
                                    QueryValue::Float(f) => *f,
                                    _ => f64::NAN,
                                };
                                af.partial_cmp(&bf).unwrap_or(Ordering::Equal)
                            })
                            .cloned()
                            .unwrap_or(QueryValue::Null),
                        AggFunc::Max => vals
                            .iter()
                            .max_by(|a, b| {
                                use std::cmp::Ordering;
                                let af = match a {
                                    QueryValue::Int(n) => *n as f64,
                                    QueryValue::Float(f) => *f,
                                    _ => f64::NAN,
                                };
                                let bf = match b {
                                    QueryValue::Int(n) => *n as f64,
                                    QueryValue::Float(f) => *f,
                                    _ => f64::NAN,
                                };
                                af.partial_cmp(&bf).unwrap_or(Ordering::Equal)
                            })
                            .cloned()
                            .unwrap_or(QueryValue::Null),
                        AggFunc::Avg => {
                            if vals.is_empty() {
                                QueryValue::Null
                            } else {
                                let sum = vals.iter().fold(0.0f64, |acc, v| match v {
                                    QueryValue::Int(n) => acc + *n as f64,
                                    QueryValue::Float(f) => acc + *f,
                                    _ => acc,
                                });
                                QueryValue::Float(sum / vals.len() as f64)
                            }
                        }
                        AggFunc::Percentile(_) | AggFunc::Median => {
                            // OQL-50: MEDIAN with even n averages the two middle values.
                            let p_val: u8 = match func {
                                AggFunc::Percentile(p) => *p,
                                _ => 50,
                            };
                            let mut fvals: Vec<f64> = vals
                                .iter()
                                .filter_map(|v| match v {
                                    QueryValue::Int(n) => Some(*n as f64),
                                    QueryValue::Float(f) => Some(*f),
                                    _ => None,
                                })
                                .collect();
                            if fvals.is_empty() {
                                QueryValue::Null
                            } else {
                                fvals.sort_by(|a, b| a.total_cmp(b));
                                let n = fvals.len();
                                if p_val == 50 && n % 2 == 0 {
                                    let mid = n / 2;
                                    QueryValue::Float((fvals[mid - 1] + fvals[mid]) / 2.0)
                                } else {
                                    let rank = ((p_val as f64 / 100.0) * n as f64).ceil() as usize;
                                    let idx = rank.saturating_sub(1).min(n - 1);
                                    QueryValue::Float(fvals[idx])
                                }
                            }
                        }
                    }
                }
                // Non-aggregate item in aggregate query.
                SelectItem::Attr(crate::query::ast::Attr::Field(f)) => filtered
                    .first()
                    .and_then(|row| {
                        col_names
                            .iter()
                            .position(|c| c == f)
                            .and_then(|pos| row.get(pos))
                            .cloned()
                    })
                    .unwrap_or(QueryValue::Null),
                _ => QueryValue::Null,
            })
            .collect();
        if plan.limit == Some(0) {
            vec![]
        } else {
            vec![agg_row]
        }
    } else {
        // Non-aggregate: project each filtered row.
        // OQL-68: when JOINs are present, defer projection to after the join so that
        // RHS columns are available for SELECT items; pre-project only when no joins.
        if plan.join_plans.is_empty() {
            filtered
                .iter()
                .map(|row| project_row_with(row, col_names))
                .collect()
        } else {
            // Pass through full VALUES rows; projection happens post-join below.
            filtered.iter().map(|row| (*row).clone()).collect()
        }
    };

    // Build an intermediate result (before JOINs, ORDER BY, and LIMIT).
    // When JOINs are present the intermediate uses the raw VALUES columns; projection
    // is applied after the join so that SELECT can reference RHS columns (OQL-68).
    let pre_join_cols: Vec<QueryColumn> = if plan.join_plans.is_empty() {
        out_cols.clone()
    } else {
        col_names
            .iter()
            .map(|c| QueryColumn { name: c.clone() })
            .collect()
    };
    let intermediate = QueryResult {
        name: String::new(),
        oql: String::new(),
        columns: pre_join_cols,
        rows,
        row_count: 0, // placeholder; will be recomputed after JOINs
        truncated: false,
        error: None,
        note: None,
        viz: None,
        elapsed_ms: None,
    };

    // Apply JOIN clauses (expands columns by appending RHS columns).
    let mut joined = apply_joins_to_result(intermediate, &plan.join_plans, None);

    // OQL-68: now that all columns from both sides are in `joined`, apply the SELECT
    // projection so that only the requested columns appear in the output.
    if !plan.join_plans.is_empty() {
        let all_col_names: Vec<String> = joined.columns.iter().map(|c| c.name.clone()).collect();
        joined.rows = joined
            .rows
            .iter()
            .map(|row| project_row_with(row, &all_col_names))
            .collect();
        joined.columns = out_cols.clone();
    }

    // Apply ORDER BY using existing multi-key sort helper.
    if let Some(order_by) = &q.order_by {
        let keys = resolve_order_by_keys(q, &joined.columns, order_by);
        sort_rows_by_columns(&mut joined.rows, &keys);
    }

    // E-05: SELECT DISTINCT on VALUES — deduplicate before OFFSET/LIMIT so that
    // the caller gets unique rows even on a VALUES-only short-circuit path.
    if q.distinct {
        let mut seen: std::collections::HashSet<String> =
            std::collections::HashSet::with_capacity(joined.rows.len());
        joined.rows.retain(|row| {
            let key: String = row
                .iter()
                .map(canonical_key)
                .collect::<Vec<_>>()
                .join("\x00");
            seen.insert(key)
        });
    }

    // Apply OFFSET first, then LIMIT (OQL-55: pre-OFFSET rows were returned).
    if let Some(off) = q.offset {
        let n = off as usize;
        if n > 0 {
            let drop = n.min(joined.rows.len());
            joined.rows.drain(..drop);
        }
    }
    let truncated = if let Some(lim) = q.limit {
        if joined.rows.len() > lim as usize {
            joined.rows.truncate(lim as usize);
            true
        } else {
            false
        }
    } else {
        false
    };

    let row_count = joined.rows.len() as u64;
    QueryResult {
        name: String::new(),
        oql: String::new(),
        columns: joined.columns,
        rows: joined.rows,
        row_count,
        truncated,
        error: None,
        note: None,
        viz: None,
        elapsed_ms: None,
    }
}

/// Evaluate an `Expr` against a VALUES row. Handles literals, field refs,
/// unary ops, binary arith, comparisons, and string methods. Returns `Null`
/// for unsupported expression types (e.g. heap attrs, subqueries).
fn eval_expr_on_row(
    expr: &crate::query::ast::Expr,
    col_names: &[String],
    row: &[crate::query::model::QueryValue],
) -> crate::query::model::QueryValue {
    use crate::query::ast::{Attr, Expr, Value};
    use crate::query::execute::{arith, unary};
    use crate::query::model::QueryValue;
    match expr {
        Expr::Lit(v) => match v {
            Value::Int(n) => QueryValue::Int(*n),
            Value::Float(f) => QueryValue::Float(*f),
            Value::Str(s) => QueryValue::Str(s.clone()),
            Value::Bool(b) => QueryValue::Bool(*b),
            Value::Null => QueryValue::Null,
        },
        Expr::Attr(Attr::Field(f)) => {
            // OQL-61: qualified column refs like `t.x` store the full dotted name;
            // strip the table alias prefix so they resolve against the projected column.
            let bare = f.rsplit_once('.').map(|(_, col)| col).unwrap_or(f.as_str());
            let lookup = if let Some(pos) = col_names.iter().position(|c| c == f) {
                Some(pos)
            } else if bare != f {
                col_names.iter().position(|c| c == bare)
            } else {
                None
            };
            lookup
                .and_then(|pos| row.get(pos))
                .cloned()
                .unwrap_or(QueryValue::Null)
        }
        Expr::Unary { op, arg } => {
            let v = eval_expr_on_row(arg, col_names, row);
            unary(*op, &v)
        }
        Expr::Binary { op, lhs, rhs } => {
            let lv = eval_expr_on_row(lhs, col_names, row);
            let rv = eval_expr_on_row(rhs, col_names, row);
            arith(&lv, *op, &rv)
        }
        Expr::Method {
            receiver,
            name,
            args,
        } => {
            let recv_val = eval_expr_on_row(receiver, col_names, row);
            eval_method_on_qv(&recv_val, name, args, col_names, row)
        }
        // OQL-66: COALESCE in JOIN ON must evaluate properly.
        Expr::Coalesce(args) => {
            for arg in args {
                let v = eval_expr_on_row(arg, col_names, row);
                if !matches!(v, QueryValue::Null) {
                    return v;
                }
            }
            QueryValue::Null
        }
        Expr::NullIf { lhs, rhs } => {
            let lv = eval_expr_on_row(lhs, col_names, row);
            let rv = eval_expr_on_row(rhs, col_names, row);
            if crate::query::execute::qv_value_eq_pub(&lv, &rv) {
                QueryValue::Null
            } else {
                lv
            }
        }
        Expr::Cast { expr, target } => {
            let v = eval_expr_on_row(expr, col_names, row);
            crate::query::execute::cast_query_value(v, *target)
        }
        Expr::Case { branches, else_ } => {
            for (pred, then_expr) in branches {
                if eval_pred_on_row(pred, col_names, row) {
                    return eval_expr_on_row(then_expr, col_names, row);
                }
            }
            else_
                .as_ref()
                .map(|e| eval_expr_on_row(e, col_names, row))
                .unwrap_or(QueryValue::Null)
        }
        // Unsupported (heap attrs, subqueries, etc.) → Null.
        _ => QueryValue::Null,
    }
}

/// Dispatch a method call on a `QueryValue` for use in VALUES query evaluation.
fn eval_method_on_qv(
    recv: &crate::query::model::QueryValue,
    name: &str,
    args: &[crate::query::ast::Expr],
    col_names: &[String],
    row: &[crate::query::model::QueryValue],
) -> crate::query::model::QueryValue {
    use crate::query::model::QueryValue;
    match recv {
        QueryValue::Str(s) => match name.to_ascii_lowercase().as_str() {
            "lower" => QueryValue::Str(s.to_lowercase()),
            "upper" => QueryValue::Str(s.to_uppercase()),
            "trim" => QueryValue::Str(s.trim().to_string()),
            "ltrim" => QueryValue::Str(s.trim_start().to_string()),
            "rtrim" => QueryValue::Str(s.trim_end().to_string()),
            "length" | "strlen" => QueryValue::Int(s.chars().count() as i64),
            "contains" if args.len() == 1 => {
                let arg = eval_expr_on_row(&args[0], col_names, row);
                match arg {
                    QueryValue::Str(pat) => QueryValue::Bool(s.contains(pat.as_str())),
                    _ => QueryValue::Null,
                }
            }
            "startswith" if args.len() == 1 => {
                let arg = eval_expr_on_row(&args[0], col_names, row);
                match arg {
                    QueryValue::Str(pat) => QueryValue::Bool(s.starts_with(pat.as_str())),
                    _ => QueryValue::Null,
                }
            }
            "endswith" if args.len() == 1 => {
                let arg = eval_expr_on_row(&args[0], col_names, row);
                match arg {
                    QueryValue::Str(pat) => QueryValue::Bool(s.ends_with(pat.as_str())),
                    _ => QueryValue::Null,
                }
            }
            _ => QueryValue::Null,
        },
        _ => QueryValue::Null,
    }
}

/// Walk a predicate tree in DFS order and materialize each `InSubquery` inner
/// by planning and executing the inner query inline. Returns one `HashSet<String>`
/// per `InSubquery` node (in DFS encounter order), keyed by the first column's
/// canonical string representation. Used by `execute_values_query` so that
/// `WHERE id IN (SELECT ...)` filters correctly even on a pure VALUES source.
fn materialize_in_subquery_sets(
    pred: &crate::query::ast::Predicate,
    out: &mut Vec<std::collections::HashSet<String>>,
) {
    use crate::query::ast::Predicate;
    match pred {
        Predicate::And(a, b) | Predicate::Or(a, b) => {
            materialize_in_subquery_sets(a, out);
            materialize_in_subquery_sets(b, out);
        }
        Predicate::Not(inner) => materialize_in_subquery_sets(inner, out),
        Predicate::InSubquery { inner, .. } => {
            // Plan and execute the inner query as a VALUES or heap query.
            let inner_result = match crate::query::plan::plan_query(inner, 1) {
                Ok(inner_plan) => {
                    if matches!(inner.from, crate::query::ast::FromSource::Values { .. }) {
                        execute_values_query(inner, &inner_plan)
                    } else {
                        // Inner is a heap query — can't execute without a dump path;
                        // return an empty set (no rows will match). The caller already
                        // falls through to the heap-scan path for heap IN subqueries.
                        crate::query::model::QueryResult {
                            name: String::new(),
                            oql: String::new(),
                            columns: Vec::new(),
                            rows: Vec::new(),
                            row_count: 0,
                            truncated: false,
                            error: None,
                            note: None,
                            viz: None,
                            elapsed_ms: None,
                        }
                    }
                }
                Err(_) => crate::query::model::QueryResult {
                    name: String::new(),
                    oql: String::new(),
                    columns: Vec::new(),
                    rows: Vec::new(),
                    row_count: 0,
                    truncated: false,
                    error: None,
                    note: None,
                    viz: None,
                    elapsed_ms: None,
                },
            };
            let set: std::collections::HashSet<String> = inner_result
                .rows
                .iter()
                .filter_map(|row| row.first())
                .map(crate::query::execute::canonical_distinct_key_pub)
                .collect();
            out.push(set);
        }
        _ => {}
    }
}

/// Evaluate a predicate against a VALUES row using precomputed IN-subquery sets.
/// `in_sets` is a slice of sets (one per `InSubquery` node, DFS order).
/// `in_cursor` is a mutable index into `in_sets`.
fn eval_pred_on_row_with_sets(
    pred: &crate::query::ast::Predicate,
    col_names: &[String],
    row: &[crate::query::model::QueryValue],
    in_sets: &[std::collections::HashSet<String>],
    in_cursor: &mut usize,
) -> bool {
    use crate::query::ast::{Attr, Predicate};
    use crate::query::execute::{canonical_distinct_key_pub as canonical_key, compare_values};
    match pred {
        Predicate::And(a, b) => {
            eval_pred_on_row_with_sets(a, col_names, row, in_sets, in_cursor)
                && eval_pred_on_row_with_sets(b, col_names, row, in_sets, in_cursor)
        }
        Predicate::Or(a, b) => {
            eval_pred_on_row_with_sets(a, col_names, row, in_sets, in_cursor)
                || eval_pred_on_row_with_sets(b, col_names, row, in_sets, in_cursor)
        }
        Predicate::Not(inner) => {
            !eval_pred_on_row_with_sets(inner, col_names, row, in_sets, in_cursor)
        }
        Predicate::Compare { lhs, op, rhs } => {
            let lv = eval_expr_on_row(lhs, col_names, row);
            let rv = eval_expr_on_row(rhs, col_names, row);
            compare_values(&lv, *op, &rv, None)
        }
        Predicate::InSubquery { lhs, .. } => {
            let idx = *in_cursor;
            *in_cursor += 1;
            let Some(set) = in_sets.get(idx) else {
                return true; // conservative: include row if set missing
            };
            // Resolve the LHS column value.
            let lhs_val = match lhs {
                Attr::Field(f) => {
                    let bare = f.rsplit_once('.').map(|(_, c)| c).unwrap_or(f.as_str());
                    col_names
                        .iter()
                        .position(|c| c == bare)
                        .and_then(|i| row.get(i))
                        .cloned()
                        .unwrap_or(crate::query::model::QueryValue::Null)
                }
                _ => crate::query::model::QueryValue::Null,
            };
            set.contains(&canonical_key(&lhs_val))
        }
        // Conservative: include row for unsupported predicate types.
        _ => true,
    }
}

/// Evaluate a predicate against a VALUES row. Returns true when the row passes.
/// Unsupported predicate types (InstanceOf, InSubquery, Exists) always return true
/// (conservative: include the row).
fn eval_pred_on_row(
    pred: &crate::query::ast::Predicate,
    col_names: &[String],
    row: &[crate::query::model::QueryValue],
) -> bool {
    use crate::query::ast::Predicate;
    use crate::query::execute::compare_values;
    match pred {
        Predicate::And(a, b) => {
            eval_pred_on_row(a, col_names, row) && eval_pred_on_row(b, col_names, row)
        }
        Predicate::Or(a, b) => {
            eval_pred_on_row(a, col_names, row) || eval_pred_on_row(b, col_names, row)
        }
        Predicate::Not(inner) => !eval_pred_on_row(inner, col_names, row),
        Predicate::Compare { lhs, op, rhs } => {
            let lv = eval_expr_on_row(lhs, col_names, row);
            let rv = eval_expr_on_row(rhs, col_names, row);
            compare_values(&lv, *op, &rv, None)
        }
        // Conservative: include row for unsupported predicate types.
        _ => true,
    }
}

/// Materialise a JOIN RHS source into `(column_names, rows)`.
fn materialise_join_source(
    source: &crate::query::ast::JoinSource,
    graph: Option<&crate::pass2::Graph>,
) -> (Vec<String>, Vec<Vec<crate::query::model::QueryValue>>) {
    use crate::query::ast::{JoinSource, Value};
    use crate::query::model::QueryValue;
    let value_to_qv = |v: &Value| -> QueryValue {
        match v {
            Value::Int(n) => QueryValue::Int(*n),
            Value::Float(f) => QueryValue::Float(*f),
            Value::Str(s) => QueryValue::Str(s.clone()),
            Value::Bool(b) => QueryValue::Bool(*b),
            Value::Null => QueryValue::Null,
        }
    };
    match source {
        JoinSource::Values { rows, columns, .. } => {
            let mat_rows: Vec<Vec<QueryValue>> = rows
                .iter()
                .map(|row| row.iter().map(&value_to_qv).collect())
                .collect();
            (columns.clone(), mat_rows)
        }
        JoinSource::VirtualTable { name, .. } => {
            let Some(g) = graph else {
                return (Vec::new(), Vec::new());
            };
            match name.to_ascii_lowercase().as_str() {
                "classes" => {
                    let cols = vec![
                        "class_name".to_string(),
                        "instance_count".to_string(),
                        "shallow_total".to_string(),
                        "retained_total".to_string(),
                    ];
                    let n = g.class_names.len();
                    let mut inst = vec![0u64; n];
                    let mut shallow = vec![0u64; n];
                    for i in 0..g.n {
                        let ci = g.class_idx[i] as usize;
                        if ci < n {
                            inst[ci] += 1;
                            shallow[ci] += g.shallow[i] as u64;
                        }
                    }
                    let mut retained = vec![0u64; n];
                    if !g.retained.is_empty() {
                        for i in 0..g.n {
                            let ci = g.class_idx[i] as usize;
                            if ci < n {
                                retained[ci] += g.retained[i];
                            }
                        }
                    }
                    let rows = (0..n)
                        .map(|ci| {
                            vec![
                                QueryValue::Str(g.class_names[ci].clone()),
                                QueryValue::Int(inst[ci] as i64),
                                QueryValue::Int(shallow[ci] as i64),
                                QueryValue::Int(retained[ci] as i64),
                            ]
                        })
                        .collect();
                    (cols, rows)
                }
                "gc_roots" => {
                    let cols = vec![
                        "dense_idx".to_string(),
                        "root_type".to_string(),
                        "root_type_name".to_string(),
                    ];
                    let rows = g
                        .gc_root_indices
                        .iter()
                        .zip(g.gc_root_types.iter())
                        .map(|(&idx, &t)| {
                            vec![
                                QueryValue::Int(idx as i64),
                                QueryValue::Int(t as i64),
                                QueryValue::Str(gc_root_type_name(t)),
                            ]
                        })
                        .collect();
                    (cols, rows)
                }
                "threads" => {
                    let cols = vec![
                        "serial".to_string(),
                        "obj_idx".to_string(),
                        "name".to_string(),
                        "is_daemon".to_string(),
                        "priority".to_string(),
                    ];
                    let rows = g
                        .thread_stacks
                        .iter()
                        .map(|ts| {
                            let props = g.thread_props.get(&ts.thread_serial);
                            vec![
                                QueryValue::Int(ts.thread_serial as i64),
                                QueryValue::Int(ts.thread_obj_idx as i64),
                                QueryValue::Str(props.map(|p| p.name.clone()).unwrap_or_default()),
                                QueryValue::Bool(props.map(|p| p.is_daemon).unwrap_or(false)),
                                QueryValue::Int(props.map(|p| p.priority as i64).unwrap_or(0)),
                            ]
                        })
                        .collect();
                    (cols, rows)
                }
                _ => (Vec::new(), Vec::new()),
            }
        }
        // Subquery JOINs not yet supported; return empty (no crash).
        JoinSource::Subquery { .. } => (Vec::new(), Vec::new()),
    }
}

fn gc_root_type_name(tag: u8) -> String {
    match tag {
        0x01 => "JNI Global".into(),
        0x02 => "JNI Local".into(),
        0x03 => "Java Frame".into(),
        0x05 => "Sticky Class".into(),
        0x08 => "Thread Object".into(),
        _ => format!("root_type_0x{tag:02x}"),
    }
}

/// Apply JOIN clauses to a materialized LHS result, returning the new (expanded)
/// result. Each JOIN appends columns from the RHS to each LHS row.
///
/// For INNER JOIN: rows with no RHS match are dropped.
/// For LEFT JOIN: rows with no RHS match get Null for all RHS columns.
pub fn apply_joins_to_result(
    mut result: crate::query::model::QueryResult,
    joins: &[crate::query::plan::JoinPlan],
    graph: Option<&crate::pass2::Graph>,
) -> crate::query::model::QueryResult {
    use crate::query::model::{QueryColumn, QueryValue};
    if joins.is_empty() {
        return result;
    }
    for join_plan in joins {
        let (rhs_cols, rhs_rows) = materialise_join_source(&join_plan.source, graph);
        if rhs_cols.is_empty() {
            // Unsupported source type or empty — for INNER JOIN filter everything out,
            // for LEFT JOIN keep LHS unchanged (append all Nulls).
            match join_plan.kind {
                crate::query::ast::JoinKind::Inner => {
                    result.rows.clear();
                    result.row_count = 0;
                }
                crate::query::ast::JoinKind::Left => {
                    // No columns to append — leave rows unchanged.
                }
            }
            continue;
        }

        // Get current LHS column names (for ON evaluation).
        let lhs_col_names: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();

        // Build hash map: rhs_key_string → Vec<rhs_row>
        // We use a string representation of the key value (instead of QueryValue directly)
        // since f64 doesn't implement Hash.
        let rhs_col_names = rhs_cols.clone();
        let qv_to_key = |qv: &crate::query::model::QueryValue| -> Option<String> {
            // OQL-61+OQL-62: NULL keys must not match in joins (SQL semantics).
            // OQL-62: use the canonical distinct key so equal Int/Float values join.
            if matches!(qv, crate::query::model::QueryValue::Null) {
                None
            } else {
                Some(crate::query::execute::canonical_distinct_key_pub(qv))
            }
        };
        let mut rhs_map: std::collections::HashMap<
            String,
            Vec<Vec<crate::query::model::QueryValue>>,
        > = std::collections::HashMap::new();
        for rhs_row in &rhs_rows {
            // NULL keys are excluded: they cannot match under SQL join semantics.
            if let Some(key) = qv_to_key(&eval_expr_on_row(
                &join_plan.on.rhs,
                &rhs_col_names,
                rhs_row,
            )) {
                rhs_map.entry(key).or_default().push(rhs_row.clone());
            }
        }

        // Expand output columns.
        let null_rhs_row: Vec<QueryValue> = rhs_cols.iter().map(|_| QueryValue::Null).collect();
        let mut new_cols: Vec<QueryColumn> = result.columns.clone();
        for c in &rhs_cols {
            new_cols.push(QueryColumn { name: c.clone() });
        }

        // Join each LHS row.
        let mut new_rows: Vec<Vec<QueryValue>> = Vec::new();
        for lhs_row in &result.rows {
            let lhs_key = qv_to_key(&eval_expr_on_row(
                &join_plan.on.lhs,
                &lhs_col_names,
                lhs_row,
            ));
            // NULL key on LHS never matches (SQL NULL semantics).
            if let Some(lhs_key) = lhs_key {
                if let Some(matching_rhs_rows) = rhs_map.get(&lhs_key) {
                    for rhs_row in matching_rhs_rows {
                        let mut combined = lhs_row.clone();
                        combined.extend_from_slice(rhs_row);
                        new_rows.push(combined);
                    }
                } else {
                    match join_plan.kind {
                        crate::query::ast::JoinKind::Inner => {
                            // No match: drop this LHS row.
                        }
                        crate::query::ast::JoinKind::Left => {
                            // No match: emit LHS + Nulls.
                            let mut combined = lhs_row.clone();
                            combined.extend_from_slice(&null_rhs_row);
                            new_rows.push(combined);
                        }
                    }
                }
            } else {
                // NULL LHS key: for LEFT JOIN emit LHS + Nulls; for INNER JOIN drop.
                if join_plan.kind == crate::query::ast::JoinKind::Left {
                    let mut combined = lhs_row.clone();
                    combined.extend_from_slice(&null_rhs_row);
                    new_rows.push(combined);
                }
            }
        }

        let row_count = new_rows.len() as u64;
        result.columns = new_cols;
        result.rows = new_rows;
        result.row_count = row_count;
    }
    result
}

/// Apply JOINs and then re-project to the query's SELECT column list.
///
/// Heap-scan queries project their LHS columns at scan time but leave the RHS
/// columns appended after JOIN; calling this mirrors the post-JOIN projection
/// step that `execute_values_query` applies.
pub fn apply_joins_and_project(
    result: crate::query::model::QueryResult,
    q: &Query,
    joins: &[crate::query::plan::JoinPlan],
    graph: Option<&crate::pass2::Graph>,
) -> crate::query::model::QueryResult {
    use crate::query::execute::query_columns;
    use crate::query::model::{QueryColumn, QueryValue};

    let mut joined = apply_joins_to_result(result, joins, graph);
    if joins.is_empty() {
        return joined;
    }

    let is_star = q.select.len() == 1 && matches!(q.select[0], crate::query::ast::SelectItem::Star);
    if is_star {
        return joined;
    }

    let out_cols: Vec<QueryColumn> = query_columns(q);
    let all_col_names: Vec<String> = joined.columns.iter().map(|c| c.name.clone()).collect();

    let project_row = |row: &[QueryValue]| -> Vec<QueryValue> {
        q.select
            .iter()
            .map(|item| match item {
                crate::query::ast::SelectItem::Attr(crate::query::ast::Attr::Field(f)) => {
                    let bare = f.rsplit_once('.').map(|(_, col)| col).unwrap_or(f.as_str());
                    let pos = all_col_names.iter().position(|c| c == f).or_else(|| {
                        if bare != f {
                            all_col_names.iter().position(|c| c == bare)
                        } else {
                            None
                        }
                    });
                    pos.and_then(|p| row.get(p))
                        .cloned()
                        .unwrap_or(QueryValue::Null)
                }
                crate::query::ast::SelectItem::Attr(crate::query::ast::Attr::RefPath {
                    hops,
                    tail,
                    role: crate::query::ast::RefRole::ProjectionOnly,
                }) if hops.len() == 1 => {
                    if let crate::query::ast::Attr::Field(col) = tail.as_ref() {
                        all_col_names
                            .iter()
                            .position(|c| c == col)
                            .and_then(|p| row.get(p))
                            .cloned()
                            .unwrap_or(QueryValue::Null)
                    } else {
                        QueryValue::Null
                    }
                }
                crate::query::ast::SelectItem::Expr(expr) => {
                    eval_expr_on_row(expr, &all_col_names, row)
                }
                _ => QueryValue::Null,
            })
            .collect()
    };

    joined.rows = joined.rows.iter().map(|row| project_row(row)).collect();
    joined.columns = out_cols;
    joined
}

pub fn run_single_dump(
    path: &str,
    queries: &[(Query, QueryPlan)],
    reachable_only: bool,
) -> std::io::Result<Vec<QueryResult>> {
    // Short-circuit VALUES-only batches: no HPROF scan needed.
    // Check if ALL queries (including all UNION/INTERSECT/EXCEPT branches) have
    // FromSource::Values so the heap scan is never needed.
    fn is_all_values(q: &Query) -> bool {
        matches!(q.from, crate::query::ast::FromSource::Values { .. })
            && q.union_branches.iter().all(is_all_values)
            && q.intersect_branches.iter().all(is_all_values)
            && q.except_branches.iter().all(is_all_values)
    }
    if queries.iter().all(|(q, _)| is_all_values(q)) {
        let has_set_ops = queries.iter().any(|(q, _)| {
            !q.union_branches.is_empty()
                || !q.intersect_branches.is_empty()
                || !q.except_branches.is_empty()
        });
        if !has_set_ops {
            // Simple case: no UNION/INTERSECT/EXCEPT branches.
            // Execute each query directly so execute_values_query handles LIMIT/OFFSET.
            let results = queries
                .iter()
                .map(|(q, plan)| execute_values_query(q, plan))
                .collect();
            return Ok(results);
        }
        // Has set-operation branches: expand/collapse is needed so all branches run.
        // Strip LIMIT/OFFSET from head-query flat slots so collapse_union_results is
        // the single applier (it reads them from the UnionGroup).
        let (flat, groups) = expand_union_queries(queries);
        let flat_results: Vec<QueryResult> = flat
            .iter()
            .enumerate()
            .map(|(i, (q, plan))| {
                // Only clear LIMIT/OFFSET on the head slot of each group (index == group.head).
                // Branch slots never carry them, but clear defensively.
                let is_group_head = groups.iter().any(|g| g.head == i);
                if is_group_head {
                    let mut q_no_limit = q.clone();
                    q_no_limit.limit = None;
                    q_no_limit.offset = None;
                    execute_values_query(&q_no_limit, plan)
                } else {
                    execute_values_query(q, plan)
                }
            })
            .collect();
        return Ok(collapse_union_results(flat_results, &groups));
    }
    let (flat, groups) = expand_union_queries(queries);
    // Propagate reachable-only into the scan so pass2 arms the per-row
    // source-index capture (only when on; otherwise nothing is allocated).
    // HistogramOnly queries are intentionally NOT demoted to SingleScan here;
    // they are skipped by Pass2 under reachable_only and re-run post-DFN with a
    // reachable-only class histogram (OQL-1 fix).
    let opts = crate::AnalyzeOptions {
        reachable_only,
        ..crate::AnalyzeOptions::default()
    };

    // Collect the inner subqueries needing an earlier pass, tagged with their
    // outer flat-slot and role (FROM identity vs IN membership on some LHS).
    let inners = collect_subquery_inners(&flat);

    if inners.is_empty() {
        // Fast path: no subqueries — one scan, no injection.
        let source = crate::source::HprofSource::from(path);
        let p1 = crate::pass1::Pass1::run(&source, false)?;
        let needs_sv = flat.iter().any(|(_, p)| p.needs.string_values);
        let needs_addr = needs_sv
            || flat
                .iter()
                .any(|(_, p)| p.finalize_at != crate::query::plan::Phase::P1);
        let addr_vec = if needs_addr {
            id_map_to_addrs(&p1.id_map)
        } else {
            Vec::new()
        };
        let mut empty = std::collections::HashMap::new();
        let mut empty_exists = std::collections::HashMap::new();
        let (g, _, shallow_c, class_idx_c, _, state, refwalk_csr, string_values, _sv_trunc) =
            crate::pass2::Pass2::build(
                &source,
                p1,
                crate::cvec::Codec::Deflate9,
                &opts,
                &flat,
                &mut empty,
                &mut empty_exists,
            )?;
        // Extract class name mapping needed for GROUP BY @displayName / @classOf.
        let class_idx: Vec<u32> = class_idx_c.restore().unwrap_or_default();
        let shallow: Vec<u32> = shallow_c.restore().unwrap_or_default();
        let class_names: Vec<String> = g
            .class_names
            .iter()
            .map(|n| crate::report::format::pretty_class_name(n))
            .collect();
        // Compute GC-reachability up front (only when reachable-only) and prune
        // each flat result by its captured source index inside the resume layer,
        // BEFORE UNION-collapse — so `@objectAddress` rows prune correctly.
        let rpo = reachable_only.then(|| {
            crate::rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets)
        });
        // OQL-1: HistogramOnly queries were deferred by Pass2 under reachable_only.
        // Run them now with a reachable-only class histogram so COUNT(*)
        // and SUM(@usedHeapSize) reflect the same population as other queries.
        let mut state = state;
        if reachable_only {
            if let Some(dfn) = rpo.as_ref().map(|r| r.dfn.as_slice()) {
                let reachable_summaries = crate::query::histogram::build_reachable_summaries(
                    &class_idx,
                    dfn,
                    None,
                    &class_names,
                );
                for (slot, r) in
                    crate::query::histogram::run_histogram_batch(&flat, &reachable_summaries)
                {
                    state.push_finished(slot, r);
                }
            }
        }
        let flat_results = resume_with_string_values(
            state,
            &flat,
            string_values,
            refwalk_csr,
            rpo.as_ref().map(|r| r.dfn.as_slice()),
            &addr_vec,
            Some(&shallow),
            &class_idx,
            &class_names,
        );
        let mut flat_results = flat_results;
        // The scan path consumed its `Pass1` into `Pass2::build`, so it has no
        // class tables (or offsets.bin) left to decode objects with. `None` here
        // makes every jsonify cell say exactly that instead of coming back as a
        // bare ObjRef — see `jsonify::NO_SESSION_NOTE`.
        crate::query::jsonify::expand_results(&mut flat_results, &flat, None);
        // Apply JOIN clauses to each flat result using the Graph from this scan,
        // then re-project to the query's SELECT list (RHS columns are appended
        // by apply_joins_to_result; projection restores the expected output shape).
        let flat_results: Vec<_> = flat_results
            .into_iter()
            .enumerate()
            .map(|(slot, result)| {
                if let Some((q, plan)) = flat.get(slot) {
                    if !plan.join_plans.is_empty() {
                        return apply_joins_and_project(result, q, &plan.join_plans, Some(&g));
                    }
                }
                result
            })
            .collect();
        let collapsed = collapse_union_results(flat_results, &groups);
        return Ok(collapsed);
    }

    // ── Inner pass: scan the dump once for all inner subqueries ──────────────
    let inner_queries: Vec<(Query, QueryPlan)> = inners
        .iter()
        .map(|i| (i.inner.clone(), i.plan.clone()))
        .collect();
    let source_inner = crate::source::HprofSource::from(path);
    let p1_inner = crate::pass1::Pass1::run(&source_inner, false)?;
    let mut empty = std::collections::HashMap::new();
    let mut empty_exists_inner = std::collections::HashMap::new();
    // Inner subqueries feed only membership/identity sets via
    // `resume_without_late_ctx`; a RefPath *inside* an inner subquery producing
    // membership is an edge case not yet wired, so the inner CSR stays discarded.
    // We DO bind the inner graph `inner_g` (for the reachability walk) because
    // reachable-only must prune the inner membership/identity sets too: an
    // unreachable object in a `... IN (SELECT ... FROM C)` set would let outer
    // rows match against a MAT-invisible object, breaking parity.
    let (inner_g, .., mut inner_state, _inner_refwalk_csr, _inner_sv, _inner_sv_trunc) =
        crate::pass2::Pass2::build(
            &source_inner,
            p1_inner,
            crate::cvec::Codec::Deflate9,
            &opts,
            &inner_queries,
            &mut empty,
            &mut empty_exists_inner,
        )?;
    // Take the per-inner-slot source-index sidecar (armed only under
    // reachable-only) BEFORE `resume_without_late_ctx` consumes the state, then
    // compute GC-reachability over the inner scan's forward CSR. `resume_*`
    // returns results in slot order (1:1 with `inner_queries`/`inners`), so
    // `inner_results[i]` is inner slot `i` and `inner_src_by_slot[&i]` its src.
    let inner_src_by_slot = inner_state.take_row_src_by_slot();
    let inner_dfn: Option<Vec<u32>> = reachable_only.then(|| {
        crate::rpo_dfs::rpo_dfs(
            inner_g.n,
            &inner_g.gc_root_indices,
            &inner_g.fwd_offsets,
            &inner_g.fwd_targets,
        )
        .dfn
    });
    let mut inner_results = crate::query::stage_runner::resume_without_late_ctx(inner_state);
    // Prune inner results to GC-reachable objects (MAT parity), keyed by the
    // scan-captured source dense index — same mechanism as the outer/fast paths,
    // so a projected `@objectAddress` prunes by exact source index, not a lossy
    // value re-read. Skipped entirely under --all (`inner_dfn` is `None`).
    if let Some(dfn) = &inner_dfn {
        for (slot, r) in inner_results.iter_mut().enumerate() {
            if let Some(src) = inner_src_by_slot.get(&slot) {
                filter_result_by_src(r, src, dfn);
            }
            // Re-apply the inner LIMIT after the reachable-only prune: the inner
            // scan deferred its LIMIT (row_src was armed so early-stop was
            // disabled), so the inner result may still exceed plan.limit after
            // pruning. Without this, FROM-subquery identity sets contain ALL
            // reachable objects instead of the intended LIMIT-N subset.
            if let Some(lim) = inners.get(slot).and_then(|i| i.plan.limit) {
                let lim = lim as usize;
                if r.rows.len() > lim {
                    r.rows.truncate(lim);
                    r.row_count = lim as u64;
                    if lim > 0 {
                        r.truncated = true;
                    }
                }
            }
        }
    }

    // ── Materialize inner results into injectable sets ───────────────────────
    // IN-subqueries → per-outer-slot address membership sets (injected into the
    // outer executors). FROM-subqueries → per-outer-slot sorted dense-index
    // sets (applied as a post-scan semi-join).
    // EXISTS-subqueries → per-outer-slot bool (did inner produce ≥1 row?).
    let mut in_sets_by_slot: std::collections::HashMap<usize, Vec<crate::query::execute::InSet>> =
        std::collections::HashMap::new();
    // outer_slot → (sorted inner dense indices, inner truncated)
    let mut from_index_by_slot: std::collections::HashMap<usize, (Vec<u32>, bool)> =
        std::collections::HashMap::new();
    // outer_slot → Vec<bool> (one per ExistsSubplan, in encounter order)
    // outer_slot → error from a failed inner subquery (propagated to the outer result)
    let mut inner_errors_by_slot: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    let mut exists_bools_by_slot: std::collections::HashMap<usize, Vec<bool>> =
        std::collections::HashMap::new();
    // outer_slot → inner row count for CountFromSubquery outer plans.
    let mut count_from_subquery_by_slot: std::collections::HashMap<usize, u64> =
        std::collections::HashMap::new();
    for (inner_idx, meta) in inners.iter().enumerate() {
        let res = &inner_results[inner_idx];
        // If the inner subquery errored (e.g. it needs @retainedHeapSize but the
        // query-only path can't build retained sizes), propagate the error to
        // the outer slot so the caller sees it instead of silently 0 rows.
        if let Some(err) = &res.error {
            inner_errors_by_slot
                .entry(meta.outer_slot)
                .or_insert_with(|| format!("inner subquery failed: {err}"));
        }
        match &meta.role {
            SubqueryRole::In => {
                let addrs: Vec<u64> = res.rows.iter().filter_map(|r| row_address(r)).collect();
                let (set, cap_trunc) =
                    build_in_subquery_set(&addrs, crate::query::SUBQUERY_SET_CAP);
                in_sets_by_slot.entry(meta.outer_slot).or_default().push(
                    crate::query::execute::InSet {
                        set,
                        truncated: cap_trunc || res.truncated,
                    },
                );
            }
            SubqueryRole::From => {
                // Check if the outer query is a CountFromSubquery — in that case
                // the outer heap scan is not needed; we just record the inner count.
                let outer_is_count = flat
                    .get(meta.outer_slot)
                    .map(|(_, p)| p.kind == crate::query::plan::StageKind::CountFromSubquery)
                    == Some(true);
                if outer_is_count {
                    count_from_subquery_by_slot.insert(meta.outer_slot, res.row_count);
                } else {
                    let mut idx: Vec<u32> =
                        res.rows.iter().filter_map(|r| row_dense_index(r)).collect();
                    idx.sort_unstable();
                    from_index_by_slot.insert(meta.outer_slot, (idx, res.truncated));
                }
            }
            SubqueryRole::Exists { negated } => {
                // EXISTS: inner produced ≥1 row → true (before negation).
                let had_rows = res.row_count > 0;
                let result = if *negated { !had_rows } else { had_rows };
                exists_bools_by_slot
                    .entry(meta.outer_slot)
                    .or_default()
                    .push(result);
            }
        }
    }

    // ── Outer pass: scan again with IN sets injected ─────────────────────────
    let source_outer = crate::source::HprofSource::from(path);
    let p1_outer = crate::pass1::Pass1::run(&source_outer, false)?;
    let needs_sv = flat.iter().any(|(_, p)| p.needs.string_values);
    let needs_addr = needs_sv
        || flat
            .iter()
            .any(|(_, p)| p.finalize_at != crate::query::plan::Phase::P1);
    let outer_addr_vec = if needs_addr {
        id_map_to_addrs(&p1_outer.id_map)
    } else {
        Vec::new()
    };
    let (
        outer_g,
        _,
        outer_shallow_c,
        outer_class_idx_c,
        _,
        outer_state,
        outer_refwalk_csr,
        outer_sv,
        _outer_sv_trunc,
    ) = crate::pass2::Pass2::build(
        &source_outer,
        p1_outer,
        crate::cvec::Codec::Deflate9,
        &opts,
        &flat,
        &mut in_sets_by_slot,
        &mut exists_bools_by_slot,
    )?;
    let outer_class_idx: Vec<u32> = outer_class_idx_c.restore().unwrap_or_default();
    let outer_shallow: Vec<u32> = outer_shallow_c.restore().unwrap_or_default();
    let outer_class_names: Vec<String> = outer_g
        .class_names
        .iter()
        .map(|n| crate::report::format::pretty_class_name(n))
        .collect();
    // Reachable-only prune happens INSIDE resume (below), keyed by each flat
    // slot's scan-captured source index, BEFORE the FROM semi-join — so the
    // sidecar stays aligned with the rows (the semi-join then operates only on
    // reachable rows). `None` under --all → prunes nothing.
    let outer_rpo = reachable_only.then(|| {
        crate::rpo_dfs::rpo_dfs(
            outer_g.n,
            &outer_g.gc_root_indices,
            &outer_g.fwd_offsets,
            &outer_g.fwd_targets,
        )
    });
    // OQL-1: add reachable-only HistogramOnly results for the outer scan too.
    let mut outer_state = outer_state;
    if reachable_only {
        if let Some(dfn) = outer_rpo.as_ref().map(|r| r.dfn.as_slice()) {
            let reachable_summaries = crate::query::histogram::build_reachable_summaries(
                &outer_class_idx,
                dfn,
                None,
                &outer_class_names,
            );
            for (slot, r) in
                crate::query::histogram::run_histogram_batch(&flat, &reachable_summaries)
            {
                outer_state.push_finished(slot, r);
            }
        }
    }
    // CountFromSubquery slots produce no executor in pass2 (nothing to scan), so
    // they never enter outer_state and resume_with_string_values would produce a
    // Vec shorter than flat.len(), causing an out-of-bounds panic when slot-indexed
    // later. Push an empty finished result for each such slot so resume produces
    // a correctly-sized Vec.
    for (slot, (_, plan)) in flat.iter().enumerate() {
        if plan.kind == crate::query::plan::StageKind::CountFromSubquery {
            outer_state.push_finished(
                slot,
                QueryResult {
                    name: String::new(),
                    oql: String::new(),
                    columns: Vec::new(),
                    rows: Vec::new(),
                    row_count: 0,
                    truncated: false,
                    error: None,
                    note: None,
                    viz: None,
                    elapsed_ms: None,
                },
            );
        }
    }
    let mut flat_results = resume_with_string_values(
        outer_state,
        &flat,
        outer_sv,
        outer_refwalk_csr,
        outer_rpo.as_ref().map(|r| r.dfn.as_slice()),
        &outer_addr_vec,
        Some(&outer_shallow),
        &outer_class_idx,
        &outer_class_names,
    );

    // ── FROM-subquery semi-join: keep only outer rows whose dense index is in
    //    the inner result set (matched by dense index). ───────────────────────
    // First: propagate any inner subquery errors to their outer slots so callers
    // see "inner subquery failed: ..." rather than a silent empty result.
    for (slot, err) in &inner_errors_by_slot {
        if let Some(r) = flat_results.get_mut(*slot) {
            r.error = Some(err.clone());
            r.rows.clear();
            r.row_count = 0;
        }
    }
    for (slot, (inner_idx_sorted, inner_trunc)) in &from_index_by_slot {
        let r = &mut flat_results[*slot];
        // Extract this outer result's own row dense indices, sorted, then
        // intersect. `intersect_from_subquery` returns the kept indices; we use
        // membership to filter the rows in place (preserving row order/shape).
        let keep: std::collections::HashSet<u32> = {
            let mut outer_idx: Vec<u32> =
                r.rows.iter().filter_map(|r| row_dense_index(r)).collect();
            outer_idx.sort_unstable();
            let (kept, _t) = intersect_from_subquery(inner_idx_sorted, *inner_trunc, &outer_idx);
            kept.into_iter().collect()
        };
        r.rows.retain(|row| {
            row_dense_index(row)
                .map(|i| keep.contains(&i))
                .unwrap_or(false)
        });
        // Apply the outer LIMIT now — deferred here because the scan could not
        // early-stop without capping pre-semi-join rows (SW-6). `retain` keeps
        // the outer scan/sort order, so this is the correct first-N / top-N.
        if let Some(limit) = flat[*slot].1.limit {
            if r.rows.len() > limit as usize {
                r.rows.truncate(limit as usize);
                // A LIMIT reached only after the semi-join is an explicit cap,
                // not lost data; leave `truncated` reflecting inner-set loss only.
            }
        }
        r.row_count = r.rows.len() as u64;
        if *inner_trunc {
            r.truncated = true;
        }
    }

    // CountFromSubquery: the outer scan produced nothing (the plan has no needs);
    // inject the inner row count directly as a single-row COUNT(*) result.
    for (slot, count) in &count_from_subquery_by_slot {
        let r = &mut flat_results[*slot];
        let (q, _) = &flat[*slot];
        let columns = crate::query::execute::query_columns(q);
        r.columns = columns;
        r.rows = vec![vec![QueryValue::Int(*count as i64)]];
        r.row_count = 1;
        r.truncated = false;
    }

    // Reachable-only pruning is fully applied by now: inner membership/identity
    // sets were pruned right after the inner pass (above), and outer rows inside
    // resume (before the FROM semi-join) — both keyed by scan-captured source
    // index. Nothing to prune here.
    //
    // jsonify runs last, after the FROM-subquery semi-join and LIMIT, so it only
    // expands rows that actually survive into the result. It degrades loudly: the
    // scan path has no pass-1 tables to decode with (see `jsonify::NO_SESSION_NOTE`).
    crate::query::jsonify::expand_results(&mut flat_results, &flat, None);
    // Apply JOIN clauses to each flat result using the outer scan's Graph, then
    // re-project to the query's SELECT list.
    let flat_results: Vec<_> = flat_results
        .into_iter()
        .enumerate()
        .map(|(slot, result)| {
            if let Some((q, plan)) = flat.get(slot) {
                if !plan.join_plans.is_empty() {
                    return apply_joins_and_project(result, q, &plan.join_plans, Some(&outer_g));
                }
            }
            result
        })
        .collect();
    Ok(collapse_union_results(flat_results, &groups))
}
/// joined by object identity) or an IN-predicate membership set (on some LHS).
enum SubqueryRole {
    From,
    In,
    /// EXISTS/NOT EXISTS: the bool result is computed from inner row count;
    /// `negated` is already encoded in the planned `ExistsSubplan` but we also
    /// carry it here so the materialization loop doesn't need to re-check.
    Exists {
        negated: bool,
    },
}

/// One inner subquery to run in the earlier pass, tagged with the outer flat-
/// slot it belongs to and its role.
struct SubqueryInner {
    outer_slot: usize,
    role: SubqueryRole,
    inner: Query,
    plan: QueryPlan,
}

/// Gather every inner subquery across the flattened outer queries. Only one
/// level deep is materialized here: a nested inner's own subqueries are planned
/// but this query subset does not run doubly-nested subqueries (the planner
/// still rejects correlation at every level).
fn collect_subquery_inners(flat: &[(Query, QueryPlan)]) -> Vec<SubqueryInner> {
    let mut out = Vec::new();
    for (slot, (_q, plan)) in flat.iter().enumerate() {
        if let Some(fp) = &plan.from_subplan {
            // The inner FROM AST lives on the outer query's FromSource; recover it.
            if let Some(inner) = _q.from.as_subquery() {
                out.push(SubqueryInner {
                    outer_slot: slot,
                    role: SubqueryRole::From,
                    inner: inner.clone(),
                    plan: (**fp).clone(),
                });
            }
        }
        for isp in &plan.in_subplans {
            out.push(SubqueryInner {
                outer_slot: slot,
                role: SubqueryRole::In,
                inner: isp.inner.clone(),
                plan: isp.plan.clone(),
            });
        }
        for esp in &plan.exists_subplans {
            out.push(SubqueryInner {
                outer_slot: slot,
                role: SubqueryRole::Exists {
                    negated: esp.negated,
                },
                inner: esp.inner.clone(),
                plan: esp.plan.clone(),
            });
        }
    }
    out
}

/// Extract the dense object index a result row identifies: `SELECT *` yields an
/// `ObjRef { index }`, `SELECT @objectId` an `Int(index)`. Rows that carry
/// neither (e.g. a scalar projection) yield `None` and never join.
pub(crate) fn row_dense_index(row: &[QueryValue]) -> Option<u32> {
    match row.first()? {
        QueryValue::ObjRef { index, .. } => Some(*index as u32),
        QueryValue::Int(i) if *i >= 0 => Some(*i as u32),
        _ => None,
    }
}

/// Prune one result's rows to GC-reachable objects using the EXACT per-row
/// source dense index `src` captured at scan time (parallel to `r.rows` before
/// this call): row `i` is kept iff `dfn[src[i]] != u32::MAX`. A dense index out
/// of `dfn`'s range is treated as unreachable. `src` shorter than `r.rows`
/// leaves the unmatched tail rows untouched (kept) — a defensive guard; in
/// practice they are always equal length. Recomputes `row_count`. Errored
/// results are left untouched. This replaces the old value-sniffing prune
/// (`row_dense_index`), which mis-read a projected `@objectAddress` (a raw heap
/// address) as a dense index and wrongly dropped every such row.
pub(crate) fn filter_result_by_src(r: &mut QueryResult, src: &[u32], dfn: &[u32]) {
    if r.error.is_some() {
        return;
    }
    let mut keep = src.iter();
    r.rows.retain(|_row| match keep.next() {
        Some(&idx) => dfn.get(idx as usize).is_some_and(|&d| d != u32::MAX),
        None => true, // no captured src for this (extra) row → keep
    });
    r.row_count = r.rows.len() as u64;
}

/// Extract the object address a result row identifies for IN-membership:
/// `SELECT @objectAddress` yields an `Int(addr)`.
/// `SELECT @objectId` yields an `ObjRef { index, .. }` — extract index as u64.
/// Non-address/non-identity rows yield `None`.
fn row_address(row: &[QueryValue]) -> Option<u64> {
    match row.first()? {
        QueryValue::Int(i) => Some(*i as u64),
        QueryValue::ObjRef { index, .. } => Some(*index),
        _ => None,
    }
}

/// Concatenate the results of homogeneous `UNION` branches (UNION ALL: no
/// dedup). The first result supplies the column headers; every branch's rows
/// are appended in branch order up to `overall_cap`, past which `truncated` is
/// set. `truncated` also propagates if any individual branch was truncated.
///
/// The cap bounds the TOTAL row count, including the head branch's own rows —
/// so a small `overall_cap` (e.g. a union-wide `LIMIT 0`) truncates the head as
/// well, not just the appended branches.
pub fn concat_union(mut branches: Vec<QueryResult>, overall_cap: usize) -> QueryResult {
    let mut out = branches.remove(0);
    // Cap the head's own rows first: the total union result may not exceed
    // `overall_cap`, and the head alone can already meet or exceed it.
    if out.rows.len() > overall_cap {
        out.rows.truncate(overall_cap);
        out.truncated = true;
    }
    for b in branches {
        out.truncated |= b.truncated;
        for row in b.rows {
            if out.rows.len() >= overall_cap {
                out.truncated = true;
                return finalize(out);
            }
            out.rows.push(row);
        }
    }
    finalize(out)
}

fn finalize(mut r: QueryResult) -> QueryResult {
    r.row_count = r.rows.len() as u64;
    r
}

/// Semi-join by dense object index for a `FROM (<subquery>)` source: keep outer
/// rows whose dense index appears in the inner query's result set. Both inputs
/// must be sorted ascending. Inner truncation propagates: a truncated inner set
/// means the membership test is incomplete, so the outer result is truncated too.
pub fn intersect_from_subquery(
    inner_sorted: &[u32],
    inner_truncated: bool,
    outer_sorted: &[u32],
) -> (Vec<u32>, bool) {
    let (mut i, mut j) = (0usize, 0usize);
    let mut out = Vec::new();
    while i < inner_sorted.len() && j < outer_sorted.len() {
        match inner_sorted[i].cmp(&outer_sorted[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(outer_sorted[j]);
                i += 1;
                j += 1;
            }
        }
    }
    (out, inner_truncated)
}

/// Build an object-address membership set from an `IN (<subquery>)` inner
/// query's projected addresses, capped at `cap` distinct entries. Returns the
/// set and whether the cap was hit (truncated — membership is then incomplete).
pub fn build_in_subquery_set(addrs: &[u64], cap: usize) -> (std::collections::HashSet<u64>, bool) {
    let mut set = std::collections::HashSet::with_capacity(addrs.len().min(cap));
    let mut truncated = false;
    for &a in addrs {
        if set.len() >= cap {
            truncated = true;
            break;
        }
        set.insert(a);
    }
    (set, truncated)
}

/// Membership test for an `IN (<subquery>)` predicate: is the outer row's LHS
/// address present in the inner result's address set?
pub fn in_subquery_contains(set: &std::collections::HashSet<u64>, lhs_addr: u64) -> bool {
    set.contains(&lhs_addr)
}

/// One original query's footprint in the flattened scan list: `count`
/// consecutive slots starting at `head`. `count == 1` for a plain query;
/// `1 + N` when the query has N `UNION` branches (head slot followed by one
/// slot per branch, in branch order). `union_limit` is the union-wide trailing
/// LIMIT (MAT gap #6), applied to the concatenated result at collapse time;
/// `None` when there is no trailing union LIMIT.
/// `distinct` mirrors the original query's `SELECT DISTINCT` flag; when true,
/// `collapse_union_results` will stable-dedup the result rows before capping.
/// `limit` is the per-query LIMIT to apply AFTER dedup (only meaningful when
/// `distinct` is true; for non-distinct queries the scan already caps rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnionGroup {
    pub head: usize,
    pub count: usize,
    pub union_limit: Option<u64>,
    pub distinct: bool,
    pub limit: Option<u64>,
    /// Rows to skip after sorting/dedup, before applying LIMIT.
    pub offset: Option<u64>,
    /// Number of INTERSECT branch slots immediately following the UNION slots.
    pub intersect_count: usize,
    /// Number of EXCEPT branch slots immediately following the INTERSECT slots.
    pub except_count: usize,
    /// ORDER BY column name and direction from the head query — used to re-sort
    /// the concatenated UNION result so the global ORDER BY is respected.
    pub order_by: Vec<(String, crate::query::ast::SortDir)>,
}

/// Flatten a caller's query list so every `UNION` branch becomes its own scan
/// slot (head first, then each branch). Branches are cloned with their own
/// `union_branches` cleared (both AST and plan) so each runs as an ordinary
/// single query through the normal execute/carry/histogram path. Returns the
/// flat `(Query, QueryPlan)` list plus, in caller order, the `UnionGroup`
/// describing how to re-collapse each original query's slots.
pub fn expand_union_queries(
    queries: &[(Query, QueryPlan)],
) -> (Vec<(Query, QueryPlan)>, Vec<UnionGroup>) {
    let mut flat: Vec<(Query, QueryPlan)> = Vec::with_capacity(queries.len());
    let mut groups: Vec<UnionGroup> = Vec::with_capacity(queries.len());
    for (q, plan) in queries {
        let head = flat.len();
        // Head slot: same query/plan but without the branch tail (branches run
        // as their own slots below).
        let mut head_q = q.clone();
        head_q.union_branches.clear();
        head_q.intersect_branches.clear();
        head_q.except_branches.clear();
        let mut head_plan = plan.clone();
        let branch_plans = std::mem::take(&mut head_plan.union_branches);
        let intersect_plans = std::mem::take(&mut head_plan.intersect_branch_plans);
        let except_plans = std::mem::take(&mut head_plan.except_branch_plans);
        flat.push((head_q, head_plan));
        // One slot per UNION branch, AST paired with its pre-planned counterpart.
        for (bq, bplan) in q.union_branches.iter().zip(branch_plans) {
            let mut bq = bq.clone();
            bq.union_branches.clear();
            flat.push((bq, bplan));
        }
        // One slot per INTERSECT branch.
        let intersect_count = q.intersect_branches.len();
        for (bq, bplan) in q.intersect_branches.iter().zip(intersect_plans) {
            let mut bq = bq.clone();
            bq.union_branches.clear();
            bq.intersect_branches.clear();
            bq.except_branches.clear();
            flat.push((bq, bplan));
        }
        // One slot per EXCEPT branch.
        let except_count = q.except_branches.len();
        for (bq, bplan) in q.except_branches.iter().zip(except_plans) {
            let mut bq = bq.clone();
            bq.union_branches.clear();
            bq.intersect_branches.clear();
            bq.except_branches.clear();
            flat.push((bq, bplan));
        }
        groups.push(UnionGroup {
            head,
            count: 1 + q.union_branches.len(),
            // Union-wide trailing LIMIT (MAT gap #6). Sourced from the plan so it
            // is applied when the branch slots are re-collapsed.
            union_limit: plan.union_limit,
            distinct: q.distinct,
            // The per-query LIMIT was cleared from the scan plan for DISTINCT queries;
            // capture it here from the AST so collapse can apply it post-dedup.
            limit: q.limit,
            offset: q.offset,
            intersect_count,
            except_count,
            // Capture the head query's ORDER BY so collapse can globally re-sort
            // the concatenated UNION result — each branch is pre-sorted but the
            // merged result is not.
            order_by: q
                .order_by
                .as_ref()
                .map(|ob| {
                    ob.keys
                        .iter()
                        .map(|(key, dir)| (crate::query::execute::attr_name(key), *dir))
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    (flat, groups)
}

/// Re-collapse flat scan results (in flattened-slot order) back to one result
/// per original query, applying `concat_union` to each `UnionGroup` that spans
/// more than one slot. `results` must be exactly the `flat` list produced by
/// [`expand_union_queries`], in the same order.
pub fn collapse_union_results(
    mut results: Vec<QueryResult>,
    groups: &[UnionGroup],
) -> Vec<QueryResult> {
    // Drain by group so slot indices stay valid regardless of per-group counts.
    let mut it = results.drain(..);
    let mut out: Vec<QueryResult> = Vec::with_capacity(groups.len());
    for g in groups {
        let branch_results: Vec<QueryResult> = (0..g.count)
            .map(|_| {
                it.next()
                    .expect("flat results shorter than groups describe")
            })
            .collect();
        let mut result = if g.count == 1 {
            branch_results.into_iter().next().unwrap()
        } else {
            // Apply the union-wide trailing LIMIT (MAT gap #6) as the row cap.
            // When ORDER BY is present, collect all rows first so the global sort
            // can pick the correct top-N after re-ordering; the LIMIT is applied
            // post-sort below. Otherwise cap early to bound memory.
            let has_order_by = !g.order_by.is_empty();
            let cap = if has_order_by {
                crate::query::OVERALL_UNION_CAP
            } else {
                match g.union_limit {
                    Some(n) => (n as usize).min(crate::query::OVERALL_UNION_CAP),
                    None => crate::query::OVERALL_UNION_CAP,
                }
            };
            concat_union(branch_results, cap)
        };
        // Re-sort the merged UNION result so the global ORDER BY is honoured.
        // Each branch was sorted independently; concatenation breaks the global
        // order. Apply the LIMIT after sorting so the top-N is correct.
        if g.count > 1 {
            if !g.order_by.is_empty() {
                let resolved: Vec<(usize, crate::query::ast::SortDir)> = g
                    .order_by
                    .iter()
                    .filter_map(|(col_name, dir)| {
                        result
                            .columns
                            .iter()
                            .position(|c| &c.name == col_name)
                            .map(|idx| (idx, *dir))
                    })
                    .collect();
                if !resolved.is_empty() {
                    crate::query::execute::sort_rows_by_columns(&mut result.rows, &resolved);
                }
                // Apply the union-wide LIMIT now that rows are globally sorted.
                if let Some(lim) = g.union_limit {
                    let lim = lim as usize;
                    if result.rows.len() > lim {
                        result.rows.truncate(lim);
                        result.truncated = true;
                        result.row_count = result.rows.len() as u64;
                    }
                }
            }
        }
        // DISTINCT: stable first-occurrence dedup on the full row tuple, then
        // apply the deferred per-query LIMIT. This path is only entered when
        // the query carried SELECT DISTINCT; non-distinct queries are untouched.
        if g.distinct {
            result = stable_dedup(result);
            if let Some(n) = g.limit {
                let n = n as usize;
                if result.rows.len() > n {
                    result.rows.truncate(n);
                    result.truncated = true;
                    result.row_count = result.rows.len() as u64;
                }
            }
        }

        // INTERSECT: drain each INTERSECT branch result and intersect row-sets.
        // E-06: use canonical_distinct_key so Int(1) == Float(1.0) and NaN is
        // handled consistently, matching COUNT DISTINCT semantics.
        for _ in 0..g.intersect_count {
            let right = it
                .next()
                .expect("flat results shorter than groups describe (intersect)");
            // Build a set of right-side row keys.
            let right_set: std::collections::HashSet<String> = right
                .rows
                .iter()
                .map(|row| row_canonical_key(row))
                .collect();
            result
                .rows
                .retain(|row| right_set.contains(&row_canonical_key(row)));
            result.truncated |= right.truncated;
        }
        // Dedup the intersect result (INTERSECT has DISTINCT semantics).
        if g.intersect_count > 0 {
            result = stable_dedup(result);
        }

        // EXCEPT: drain each EXCEPT branch result and subtract row-sets.
        for _ in 0..g.except_count {
            let right = it
                .next()
                .expect("flat results shorter than groups describe (except)");
            let right_set: std::collections::HashSet<String> = right
                .rows
                .iter()
                .map(|row| row_canonical_key(row))
                .collect();
            result
                .rows
                .retain(|row| !right_set.contains(&row_canonical_key(row)));
            result.truncated |= right.truncated;
        }
        // Dedup the except result (EXCEPT has DISTINCT semantics).
        if g.except_count > 0 {
            result = stable_dedup(result);
        }

        // Recompute row_count after any set operations.
        if g.intersect_count > 0 || g.except_count > 0 {
            result.row_count = result.rows.len() as u64;
        }

        // OFFSET: skip the first n rows (applied after all sorting/dedup/set ops).
        if let Some(n) = g.offset {
            let n = n as usize;
            if n > 0 && !result.rows.is_empty() {
                let drop = n.min(result.rows.len());
                result.rows.drain(..drop);
                result.row_count = result.rows.len() as u64;
            }
        }

        // LIMIT: re-apply the user LIMIT after OFFSET (when OFFSET is present the
        // scan may have collected more rows than limit to account for skipping).
        if g.offset.is_some() {
            if let Some(n) = g.limit {
                let n = n as usize;
                if result.rows.len() > n {
                    result.rows.truncate(n);
                    result.truncated = true;
                    result.row_count = result.rows.len() as u64;
                }
            }
        }

        out.push(result);
    }
    out
}

/// Stable first-occurrence row dedup: keep the first row for each unique key,
/// preserving original order. Uses `Debug` formatting as a total canonical key
/// (handles `Float(NaN)` without panic; allocation is acceptable since dedup is
/// query-result post-processing, not per-object hot path).
/// Canonical key for a result row: joins each value's canonical_distinct_key
/// with a separator. Ensures Int(1) == Float(1.0) and 0.0 == -0.0.
fn row_canonical_key(row: &[QueryValue]) -> String {
    row.iter()
        .map(canonical_distinct_key_pub)
        .collect::<Vec<_>>()
        .join("\x00")
}

fn stable_dedup(mut r: QueryResult) -> QueryResult {
    use std::collections::HashSet;
    let mut seen: HashSet<String> = HashSet::with_capacity(r.rows.len());
    r.rows.retain(|row| seen.insert(row_canonical_key(row)));
    r.row_count = r.rows.len() as u64;
    r
}

/// Warm REPL cache: heap scanned ONCE, resident tables kept alive so
/// resident-only queries (see `QueryPlan::is_resident_only`) can be re-served
/// without re-scanning. REPL-ONLY — never used by the one-shot query/analyze
/// paths, which keep a byte/RSS-identity contract.
pub struct ReplCache {
    pub p1: crate::pass1::Pass1,
    pub n: usize,
    pub shallow: Vec<u32>,
    pub class_ids: Vec<u32>,
    pub dfn: Option<Vec<u32>>,
    pub id_size: usize,
    pub reachable_only: bool,
    pub source: crate::source::HprofSource,
    /// Dense-object-index → class-histogram row (same mapping as Pass2.class_idx).
    /// Used to resolve `@classOf`/`@displayName` in the REPL cached query path.
    pub class_idx: Vec<u32>,
    /// Class-histogram row names (indexed by class_idx values).
    pub class_names: Vec<String>,
    /// Per-dense-index byte offsets of each object's HPROF record in the
    /// decompressed stream, captured during pass1. Used by jsonify() and array
    /// subscript to seek directly to any object without re-scanning the dump.
    /// Empty when the dump was loaded without offset capture (should not happen
    /// in practice — `build_with_progress` always sets `capture_hprof_offsets`).
    pub offsets: Vec<u64>,
}

impl ReplCache {
    pub fn build(
        source: &crate::source::HprofSource,
        reachable_only: bool,
    ) -> std::io::Result<ReplCache> {
        Self::build_with_progress(source, reachable_only, &mut |_, _| {})
    }

    /// Like `build` but fires `progress(phase, fraction)` between phases so
    /// callers can update a UI progress bar.  Called at:
    ///   - `("pass1_a", 1.0)` after the first Pass1 (for p1_owned)
    ///   - `("pass1_b", 1.0)` after the second Pass1 (for Pass2)
    ///   - `("pass2",   1.0)` after Pass2 completes
    pub fn build_with_progress(
        source: &crate::source::HprofSource,
        reachable_only: bool,
        progress: &mut dyn FnMut(&str, f32),
    ) -> std::io::Result<ReplCache> {
        let opts = crate::AnalyzeOptions {
            reachable_only,
            ..crate::AnalyzeOptions::default()
        };
        let source_cache = source.clone();
        let mut p1_owned = crate::pass1::Pass1::run(&source_cache, true)?;
        let offsets = std::mem::take(&mut p1_owned.hprof_offsets);
        progress("pass1_a", 1.0);
        let p1_for_pass2 = crate::pass1::Pass1::run(&source_cache, false)?;
        progress("pass1_b", 1.0);
        let flat: Vec<(Query, QueryPlan)> = Vec::new();
        let mut empty = std::collections::HashMap::new();
        let mut empty_exists = std::collections::HashMap::new();
        let (g, _, shallow_c, class_idx_c, ..) = crate::pass2::Pass2::build(
            &source_cache,
            p1_for_pass2,
            crate::cvec::Codec::Deflate9,
            &opts,
            &flat,
            &mut empty,
            &mut empty_exists,
        )?;
        progress("pass2", 1.0);
        let n = g.n;
        // g.shallow / g.class_idx are emptied during build (compressed). Restore.
        let shallow: Vec<u32> = shallow_c.restore()?;
        let class_idx: Vec<u32> = class_idx_c.restore()?;
        let class_names: Vec<String> = g
            .class_names
            .iter()
            .map(|n| crate::report::format::pretty_class_name(n))
            .collect();
        let class_ids = p1_owned.class_ids.clone();
        let dfn = if reachable_only {
            Some(
                crate::rpo_dfs::rpo_dfs(g.n, &g.gc_root_indices, &g.fwd_offsets, &g.fwd_targets)
                    .dfn,
            )
        } else {
            None
        };
        let id_size = p1_owned.id_size as usize;
        let cache = ReplCache {
            p1: p1_owned,
            n,
            shallow,
            class_ids,
            class_idx,
            class_names,
            dfn,
            id_size,
            reachable_only,
            source: source.clone(),
            offsets,
        };
        // Write offsets.bin to the default cache dir so future `heap query` runs
        // can use jsonify without a prior `heap load`. Ignore errors — the query
        // still works via the in-memory offsets captured above.
        #[cfg(feature = "native")]
        if let Ok(cd) = crate::cache::CacheDir::for_dump(std::path::Path::new(source.file_path())) {
            if !cd.has_hprof_offsets() && !cache.offsets.is_empty() {
                let _ = cd.write_hprof_offsets(&cache.offsets);
            }
        }
        Ok(cache)
    }

    /// Scan the HPROF source and decode all String instance values.
    /// Used by the resident-only query path when `toString(s)` is requested
    /// (that path uses empty blobs so can't capture `value` field pointers
    /// from the cache; this re-scans the file once on demand).
    pub fn build_string_values(&self) -> std::io::Result<std::collections::HashMap<u32, String>> {
        use crate::query::stringvals::{STRING_VALUES_CAP, StringCapture};
        use crate::types::HprofType;

        let resolver = LiveResolver::new(
            &self.p1.class_map,
            &self.p1.strings,
            self.id_size,
            &self.p1.id_map,
            &self.shallow,
        );
        let id_size = self.id_size as u8;
        let ref_width = resolver.ref_width();

        // Collect (value_off, coder_off) for each String class_id we encounter,
        // memoized so we compute it at most once per class.
        let mut off_cache: std::collections::HashMap<u64, Option<(usize, Option<usize>)>> =
            std::collections::HashMap::new();
        let mut capture = StringCapture::new(STRING_VALUES_CAP);
        let id_map = &self.p1.id_map;

        let source = self.source.clone();
        let open = move || source.open();
        crate::pass2::scan_all_instances(&open, id_size, |obj_addr, class_id, blob| {
            let offs = *off_cache.entry(class_id).or_insert_with(|| {
                let value_off = match resolver.field(class_id, "value") {
                    Some((off, HprofType::Object)) => off as usize,
                    _ => return None,
                };
                let coder_off = resolver
                    .field(class_id, "coder")
                    .filter(|&(_, ty)| ty == HprofType::Byte)
                    .map(|(off, _)| off as usize);
                Some((value_off, coder_off))
            });
            let Some((value_off, coder_off)) = offs else {
                return;
            };
            if value_off + ref_width > blob.len() {
                return;
            }
            let mut arr_addr: u64 = 0;
            for &b in &blob[value_off..value_off + ref_width] {
                arr_addr = (arr_addr << 8) | b as u64;
            }
            if arr_addr == 0 {
                // Java "" has a null/zero backing array; record it as empty string
                // so toString("") returns "" instead of null.
                if let Some(dense_idx) = id_map.index_of(obj_addr) {
                    capture.insert_empty(dense_idx as u32);
                }
                return;
            }
            let coder = match coder_off {
                Some(co) if co < blob.len() => blob[co],
                _ => 1,
            };
            // Look up the dense index for this object address.
            if let Some(dense_idx) = id_map.index_of(obj_addr) {
                capture.insert(dense_idx as u32, arr_addr, coder);
            }
        })?;

        let source2 = self.source.clone();
        capture.decode_all(move || source2.open(), id_size)
    }

    /// Scan all instances and record which (owner_class, field_label) holds
    /// a reference to each String object in `string_dense_idxs`.
    ///
    /// `string_values` is the dense_idx→text map so that when a Map Entry's
    /// `key` field points to a String, we can use its text as the field label
    /// instead of the generic "value", producing attribution like
    /// `(java.util.HashMap, apiKey)` rather than `(HashMap$Entry, value)`.
    ///
    /// Also scans OBJ_ARRAY_DUMP records so that the backing `table` arrays of
    /// HashMap / Hashtable / Properties are traversed.
    ///
    /// Returns `HashMap<string_dense_idx, Vec<(owner_class_name, field_label)>>`.
    // Called from main.rs (binary), not from the lib — suppress spurious dead_code lint.
    #[allow(dead_code)]
    pub fn build_string_referrers(
        &self,
        string_dense_idxs: &std::collections::HashSet<u32>,
        string_values: &std::collections::HashMap<u32, String>,
    ) -> std::io::Result<std::collections::HashMap<u32, Vec<(String, String)>>> {
        use crate::pass2::Record;
        use crate::types::HprofType;

        let id_size = self.id_size as u8;
        let ref_width = self.id_size;
        let id_map = &self.p1.id_map;
        let class_map = &self.p1.class_map;
        let strings = &self.p1.strings;

        // Resolve the dot-form name for each class.
        let mut class_names: std::collections::HashMap<u64, String> =
            std::collections::HashMap::with_capacity(class_map.len());
        for (&addr, ci) in class_map {
            if let Some(raw) = strings.get(&ci.name_id) {
                class_names.insert(addr, raw.replace('/', "."));
            }
        }

        // String class IDs — skip attributing String→String references.
        let string_class_ids: std::collections::HashSet<u64> = class_names
            .iter()
            .filter(|(_, n)| *n == "java.lang.String")
            .map(|(&id, _)| id)
            .collect();

        // Map entry class IDs — these need special key→value attribution.
        // Covers HashMap$Entry, LinkedHashMap$Entry, Hashtable$Entry,
        // ConcurrentHashMap$Node, WeakHashMap$Entry, LinkedList$Node,
        // Properties (inherits Hashtable), and similar naming patterns.
        let is_map_entry = |name: &str| -> bool {
            let s = name.to_ascii_lowercase();
            s.contains("$entry")
                || s.contains("$node")
                || s.contains("mapentry")
                || (s.contains("entry")
                    && (s.contains("map") || s.contains("hash") || s.contains("table")))
        };

        let field_type_width = |t: HprofType| -> usize {
            match t {
                HprofType::Object => ref_width,
                HprofType::Boolean | HprofType::Byte => 1,
                HprofType::Char | HprofType::Short => 2,
                HprofType::Float | HprofType::Int => 4,
                HprofType::Double | HprofType::Long => 8,
            }
        };

        // Compute all Object-typed (field_name, byte_offset) pairs for a class,
        // walking the super-chain (subclass fields first, matching HPROF layout).
        let compute_obj_fields = |class_id: u64| -> Option<Vec<(String, usize)>> {
            let mut chain: Vec<u64> = Vec::new();
            let mut cur = class_id;
            loop {
                chain.push(cur);
                match class_map.get(&cur) {
                    Some(ci) if ci.super_id != 0 => cur = ci.super_id,
                    _ => break,
                }
            }
            let mut fields: Vec<(String, usize)> = Vec::new();
            let mut offset = 0usize;
            for caddr in &chain {
                let ci = class_map.get(caddr)?;
                for &(fname_id, ftype) in &ci.fields {
                    let w = field_type_width(ftype);
                    if ftype == HprofType::Object {
                        let fname = strings
                            .get(&fname_id)
                            .cloned()
                            .unwrap_or_else(|| fname_id.to_string());
                        fields.push((fname, offset));
                    }
                    offset += w;
                }
            }
            Some(fields)
        };

        // Helper: read a ref-width big-endian address from blob[offset..].
        let read_ref = |blob: &[u8], offset: usize| -> Option<u64> {
            if offset + ref_width > blob.len() {
                return None;
            }
            let mut addr: u64 = 0;
            for &b in &blob[offset..offset + ref_width] {
                addr = (addr << 8) | b as u64;
            }
            if addr == 0 { None } else { Some(addr) }
        };

        // Helper: look up whether an object address is a known String dense_idx,
        // and if so return its text.
        let addr_to_string = |addr: u64| -> Option<(u32, &str)> {
            let didx = id_map.index_of(addr)? as u32;
            if string_dense_idxs.contains(&didx) {
                Some((
                    didx,
                    string_values.get(&didx).map(String::as_str).unwrap_or(""),
                ))
            } else {
                None
            }
        };

        let mut field_cache: std::collections::HashMap<u64, Option<Vec<(String, usize)>>> =
            std::collections::HashMap::new();

        // Phase 1: single-pass scan with scan_all_records.
        // For each Instance: attribute Object-typed fields to string referents.
        //   - If the instance is a Map Entry: read `key` and `value` fields,
        //     and if key is a String, label the value attribution as that key text.
        //   - Otherwise: use the class-name + field-name as attribution.
        // For each ObjArray: iterate element references and attribute each
        //   String element as `(array_class_short_name, [n])`.
        let mut result: std::collections::HashMap<u32, Vec<(String, String)>> =
            std::collections::HashMap::new();

        let source = self.source.clone();
        let open = move || source.open();
        crate::pass2::scan_all_records(&open, id_size, |record| {
            match record {
                Record::Instance(_obj_addr, class_id, blob) => {
                    if string_class_ids.contains(&class_id) {
                        return;
                    }
                    let owner_class = match class_names.get(&class_id) {
                        Some(n) => n,
                        None => return,
                    };

                    // For Map entry types, try to read key→value and produce
                    // attribution like (HashMap, "apiKey") for the value String.
                    if is_map_entry(owner_class) {
                        let obj_fields = field_cache
                            .entry(class_id)
                            .or_insert_with(|| compute_obj_fields(class_id));
                        let fields = match obj_fields {
                            Some(f) => f,
                            None => return,
                        };
                        // Find "key" and "value" fields.
                        let mut key_addr: Option<u64> = None;
                        let mut val_addr: Option<u64> = None;
                        for (fname, offset) in fields.iter() {
                            if fname == "key" {
                                key_addr = read_ref(blob, *offset);
                            } else if fname == "value" {
                                val_addr = read_ref(blob, *offset);
                            }
                        }
                        if let Some(va) = val_addr {
                            if let Some((val_didx, _)) = addr_to_string(va) {
                                // Label: use the key string text if available, else "value".
                                let label = key_addr
                                    .and_then(addr_to_string)
                                    .map(|(_, kt)| kt.to_owned())
                                    .unwrap_or_else(|| "value".to_owned());
                                // Owner: strip inner class suffix for readability,
                                // e.g. "java.util.HashMap$Node" → "Map[...]"
                                let container = owner_class
                                    .split('$')
                                    .next()
                                    .unwrap_or(owner_class)
                                    .to_owned();
                                result.entry(val_didx).or_default().push((container, label));
                            }
                        }
                        // Also check if the key itself is a known String
                        // (e.g. scanning a Set of secrets, or key contains secret text).
                        if let Some(ka) = key_addr {
                            if let Some((key_didx, _)) = addr_to_string(ka) {
                                let container = owner_class
                                    .split('$')
                                    .next()
                                    .unwrap_or(owner_class)
                                    .to_owned();
                                result
                                    .entry(key_didx)
                                    .or_default()
                                    .push((container, "key".to_owned()));
                            }
                        }
                        return;
                    }

                    // Generic instance: attribute each Object field.
                    let obj_fields = field_cache
                        .entry(class_id)
                        .or_insert_with(|| compute_obj_fields(class_id));
                    let fields = match obj_fields {
                        Some(f) => f,
                        None => return,
                    };
                    for (field_name, offset) in fields.iter() {
                        if let Some(ref_addr) = read_ref(blob, *offset) {
                            if let Some((didx, _)) = addr_to_string(ref_addr) {
                                result
                                    .entry(didx)
                                    .or_default()
                                    .push((owner_class.clone(), field_name.clone()));
                            }
                        }
                    }
                }
                Record::ObjArray(addr, array_class_id, _count, elem_bytes) => {
                    // Attribute each String element in an object array.
                    // Only emit attribution when the array itself has a meaningful class name
                    // (skip raw Object[] used as generic backing storage unless we know the
                    // parent container — that would require two-pass resolution).
                    let arr_class = class_names
                        .get(&array_class_id)
                        .map(String::as_str)
                        .unwrap_or("");
                    // Skip java.lang.Object[] — too generic; noise ratio is high.
                    if arr_class == "java.lang.Object[]" || arr_class.is_empty() {
                        return;
                    }
                    let _ = addr; // array address unused for now
                    let mut off = 0usize;
                    while off + ref_width <= elem_bytes.len() {
                        let mut ref_addr: u64 = 0;
                        for &b in &elem_bytes[off..off + ref_width] {
                            ref_addr = (ref_addr << 8) | b as u64;
                        }
                        if ref_addr != 0 {
                            if let Some((didx, _)) = addr_to_string(ref_addr) {
                                result
                                    .entry(didx)
                                    .or_default()
                                    .push((arr_class.to_owned(), "[]".to_owned()));
                            }
                        }
                        off += ref_width;
                    }
                }
                Record::PrimArray(..) => {}
            }
        })?;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse::parse;
    use crate::query::plan::plan_query;

    /// Parse+plan+optimize an OQL string exactly as the REPL's `run_one` does,
    /// so plans match the scan path bit-for-bit.
    fn parse_plan_opt(oql: &str) -> (crate::query::ast::Query, QueryPlan) {
        let q = crate::query::parse::parse_or_report(oql).expect("parse");
        let plan = crate::query::plan::plan_query(&q, 0).expect("plan");
        let plan = crate::query::optimize::optimize(
            plan,
            &q,
            &crate::query::optimize::SchemaStats::default(),
        );
        (q, plan)
    }

    /// Collect a canonical set of row-identifying values from a set of results:
    /// object refs by dense index, and scalars tagged distinctly so a COUNT(*)
    /// or @objectId value never collides with an ObjRef index.
    fn addr_set(results: &[QueryResult]) -> std::collections::BTreeSet<u64> {
        let mut s = std::collections::BTreeSet::new();
        for r in results {
            for row in &r.rows {
                for v in row {
                    match v {
                        QueryValue::ObjRef { index, .. } => {
                            s.insert(*index);
                        }
                        QueryValue::Int(i) => {
                            s.insert(*i as u64 | (1u64 << 62));
                        }
                        _ => {}
                    }
                }
            }
        }
        s
    }

    #[test]
    fn resident_only_matches_scan_path() {
        let path = "tests/fixtures/dump_4_philosophers.hprof";
        let cache = ReplCache::build(&crate::source::HprofSource::from(path), true).expect("cache");

        // Aggregate queries (COUNT(*)) are intentionally excluded here: the scan
        // path computes DFN only after the full scan completes, so aggregate
        // executors (which never capture per-row source indices) cannot be
        // retroactively filtered by reachability. That known limitation is tested
        // separately in aggregate_reachability_consistent_with_filter.
        for oql in [
            "SELECT @objectAddress FROM java.lang.Thread",
            "SELECT @objectAddress FROM INSTANCEOF java.lang.Thread",
        ] {
            let (q, plan) = parse_plan_opt(oql);
            assert!(plan.is_resident_only(), "{oql} must be resident-only");

            let cache_out =
                run_resident_only(&cache, &[(q.clone(), plan.clone())], true).expect("cache run");
            let scan_out = run_single_dump(path, &[(q, plan)], true).expect("scan run");

            assert_eq!(cache_out.len(), scan_out.len(), "result count for {oql}");
            assert_eq!(
                cache_out[0].row_count, scan_out[0].row_count,
                "row_count for {oql}: cache={} scan={}",
                cache_out[0].row_count, scan_out[0].row_count
            );
            assert_eq!(
                addr_set(&cache_out),
                addr_set(&scan_out),
                "row-value set parity for {oql}"
            );
        }
    }

    #[test]
    fn resident_only_matches_scan_path_more() {
        let path = "tests/fixtures/dump_4_philosophers.hprof";
        let cache = ReplCache::build(&crate::source::HprofSource::from(path), true).expect("cache");

        for oql in [
            "SELECT * FROM java.lang.Thread",
            "SELECT classof(t) FROM java.lang.Thread t",
            "SELECT @objectId FROM java.lang.String",
            "SELECT @objectAddress FROM java.lang.Thread \
             UNION SELECT @objectAddress FROM java.util.HashMap",
        ] {
            let (q, plan) = parse_plan_opt(oql);
            assert!(
                plan.is_resident_only(),
                "{oql} is expected resident-only for this test"
            );

            let cache_out =
                run_resident_only(&cache, &[(q.clone(), plan.clone())], true).expect("cache run");
            let scan_out = run_single_dump(path, &[(q, plan)], true).expect("scan run");

            assert_eq!(
                cache_out.len(),
                scan_out.len(),
                "result count for {oql}: cache={} scan={}",
                cache_out.len(),
                scan_out.len()
            );
            assert_eq!(
                cache_out[0].row_count, scan_out[0].row_count,
                "row_count for {oql}: cache={} scan={}",
                cache_out[0].row_count, scan_out[0].row_count
            );
            assert_eq!(
                addr_set(&cache_out),
                addr_set(&scan_out),
                "row-value set parity for {oql}"
            );
        }
    }

    #[test]
    fn aggregate_reachability_consistent_with_filter() {
        // COUNT(*) with --reachable-only must return the same count as a
        // row-mode query filtered to the same set. Before the fix, aggregate
        // executors ignored the reachability pre-filter and counted ALL objects.
        let path = "tests/fixtures/dump_4_philosophers.hprof";
        let meta = std::fs::metadata(path);
        match meta {
            Ok(m) if m.len() >= 1024 => {}
            _ => return, // fixture absent (CI without LFS)
        }
        let cache = ReplCache::build(&crate::source::HprofSource::from(path), true).expect("cache");

        let (q_count, plan_count) = parse_plan_opt("SELECT COUNT(*) FROM java.lang.String");
        let (q_rows, plan_rows) = parse_plan_opt("SELECT @objectId FROM java.lang.String");

        let count_out =
            run_resident_only(&cache, &[(q_count, plan_count)], true).expect("count run");
        let rows_out = run_resident_only(&cache, &[(q_rows, plan_rows)], true).expect("rows run");

        let count_val = match &count_out[0].rows[0][0] {
            QueryValue::Int(n) => *n as u64,
            other => panic!("expected Int count, got {other:?}"),
        };
        assert_eq!(
            count_val, rows_out[0].row_count,
            "COUNT(*) ({count_val}) must match row-mode count ({}) with --reachable-only",
            rows_out[0].row_count
        );

        // With --all (no reachability filter), both must agree on the larger total.
        let (q_count2, plan_count2) = parse_plan_opt("SELECT COUNT(*) FROM java.lang.String");
        let (q_rows2, plan_rows2) = parse_plan_opt("SELECT @objectId FROM java.lang.String");
        let count_all =
            run_resident_only(&cache, &[(q_count2, plan_count2)], false).expect("count-all run");
        let rows_all =
            run_resident_only(&cache, &[(q_rows2, plan_rows2)], false).expect("rows-all run");
        let count_all_val = match &count_all[0].rows[0][0] {
            QueryValue::Int(n) => *n as u64,
            other => panic!("expected Int, got {other:?}"),
        };
        assert_eq!(
            count_all_val, rows_all[0].row_count,
            "COUNT(*) and row-mode must agree with --all too"
        );
        // --all should return >= reachable-only (unreachable objects are a superset).
        assert!(
            count_all_val >= count_val,
            "--all count ({count_all_val}) must be >= reachable-only count ({count_val})"
        );
    }

    #[test]
    fn repl_cache_builds_from_fixture() {
        let cache = ReplCache::build(
            &crate::source::HprofSource::from("tests/fixtures/dump_4_philosophers.hprof"),
            true,
        )
        .expect("cache build");
        assert!(cache.n > 0, "some objects");
        assert_eq!(cache.shallow.len(), cache.n, "shallow covers all objects");
        assert_eq!(
            cache.class_ids.len(),
            cache.n,
            "class_ids covers all objects"
        );
        assert!(cache.dfn.is_some(), "reachable-only build computes dfn");

        // --all build: no dfn.
        let raw = ReplCache::build(
            &crate::source::HprofSource::from("tests/fixtures/dump_4_philosophers.hprof"),
            false,
        )
        .expect("raw");
        assert!(raw.dfn.is_none(), "raw build has no dfn");
    }

    #[test]
    fn resident_only_tostring_returns_non_null() {
        let path = "tests/fixtures/dump_4_philosophers.hprof";
        let cache = ReplCache::build(&crate::source::HprofSource::from(path), true).expect("cache");

        // toString(s) needs string_values — resident path must re-scan to get them.
        let oql = "SELECT toString(s) FROM java.lang.String s LIMIT 10";
        let (q, plan) = parse_plan_opt(oql);

        assert!(
            plan.needs.string_values,
            "plan must need string_values for this query"
        );
        let sv = cache.build_string_values().expect("build_string_values");
        assert!(
            !sv.is_empty(),
            "build_string_values must return a non-empty map"
        );

        let results = run_resident_only(&cache, &[(q, plan)], true).expect("run");
        let result = &results[0];
        // At least some strings must be non-null (not every String has a value array, but most do).
        let non_null = result
            .rows
            .iter()
            .filter(|row| row.iter().any(|v| !matches!(v, QueryValue::Null)))
            .count();
        assert!(
            non_null > 0,
            "expected at least some non-null toString values, got 0 out of {}",
            result.row_count
        );
    }

    #[test]
    fn tostring_with_retained_heap_size_non_null_and_sorted() {
        let path = "tests/fixtures/dump_4_philosophers.hprof";
        let source = crate::source::HprofSource::from(path);
        let cache = ReplCache::build(&source, true).expect("cache");

        // Combined carry-mode query: toString needs string_values, @retainedHeapSize needs retained.
        let oql = "SELECT toString(s) AS value, @retainedHeapSize AS bytes FROM java.lang.String s ORDER BY bytes DESC LIMIT 10";
        let (q, plan) = parse_plan_opt(oql);

        assert!(plan.needs.string_values, "must need string_values");
        assert!(plan.needs.retained, "must need retained");

        let retained = {
            let (_report, ret) =
                crate::analyze_to_report_with_retained(&source, &crate::AnalyzeOptions::default())
                    .expect("analyze");
            ret
        };
        let results =
            run_resident_with_retained(&cache, &[(q, plan)], true, &retained).expect("run");
        let result = &results[0];

        assert!(
            result.error.is_none(),
            "unexpected error: {:?}",
            result.error
        );
        assert!(result.row_count > 0, "expected rows");

        // All @retainedHeapSize values must be non-null and non-zero.
        let bytes_col = result
            .columns
            .iter()
            .position(|c| c.name == "bytes")
            .expect("bytes column");
        for row in &result.rows {
            let v = &row[bytes_col];
            assert!(!matches!(v, QueryValue::Null), "bytes must not be null");
            if let QueryValue::Int(n) = v {
                assert!(*n > 0, "retained heap size must be positive, got {n}");
            }
        }

        // Rows must be sorted descending by bytes.
        let vals: Vec<i64> = result
            .rows
            .iter()
            .map(|r| {
                if let QueryValue::Int(n) = r[bytes_col] {
                    n
                } else {
                    0
                }
            })
            .collect();
        for w in vals.windows(2) {
            assert!(w[0] >= w[1], "rows not sorted DESC: {} < {}", w[0], w[1]);
        }
    }

    #[test]
    fn object_address_in_carry_mode_is_nonzero() {
        // Regression: @objectAddress returned 0 for all rows when toString(s) was
        // present in SELECT, because the file-scan path never passed the addr_vec
        // to resume_with_string_values.
        let path = "tests/fixtures/dump_4_philosophers.hprof";
        let (q, plan) = parse_plan_opt(
            "SELECT @objectAddress, toString(s) AS value FROM java.lang.String s LIMIT 5",
        );
        assert!(plan.needs.string_values, "must be carry mode");

        let results =
            crate::query::run::run_single_dump(path, &[(q, plan)], false).expect("run_single_dump");
        let result = &results[0];
        assert!(
            result.error.is_none(),
            "unexpected error: {:?}",
            result.error
        );
        assert!(result.row_count > 0, "expected rows");

        let addr_col = result
            .columns
            .iter()
            .position(|c| c.name == "@objectAddress")
            .expect("@objectAddress column");
        for row in &result.rows {
            if let QueryValue::Int(addr) = row[addr_col] {
                assert!(addr != 0, "@objectAddress must not be 0 in carry mode");
            }
        }
    }

    /// Minimal `ClassResolver` mapping a couple of class addresses to names;
    /// fields are unused by these class-only queries.
    struct FakeResolver {
        names: HashMap<u64, String>,
    }

    impl ClassResolver for FakeResolver {
        fn class_name(&self, class_id: u64) -> Option<&str> {
            self.names.get(&class_id).map(String::as_str)
        }
    }

    fn pq(q: &crate::query::ast::Query) -> crate::query::plan::QueryPlan {
        plan_query(q, crate::query::DEFAULT_PATH_DEPTH_CAP).unwrap()
    }

    #[test]
    fn scan_driver_fans_out_and_finish_tags_name_and_oql() {
        let resolver = FakeResolver {
            names: [
                (10u64, "com.acme.Foo".to_string()),
                (20u64, "com.acme.Bar".to_string()),
            ]
            .into_iter()
            .collect(),
        };

        // Two independent SingleScan queries over the same fake resolver.
        let q_foo = parse("SELECT @objectId FROM com.acme.Foo").unwrap();
        let p_foo = pq(&q_foo);
        let q_bar = parse("SELECT @objectId FROM com.acme.Bar").unwrap();
        let p_bar = pq(&q_bar);

        let entries = vec![
            (0usize, SingleScanExecutor::new(&q_foo, &p_foo, &resolver)),
            (1usize, SingleScanExecutor::new(&q_bar, &p_bar, &resolver)),
        ];
        let mut driver = ScanDriver::new(entries);
        assert!(!driver.is_empty());

        // Drive a few objects: two Foo (class 10), one Bar (class 20).
        driver.visit_instance(1, 10, &[]);
        driver.visit_instance(2, 20, &[]);
        driver.visit_instance(3, 10, &[]);

        let state = driver.finish_state();

        // Both queries are Phase-1 (no @retainedHeapSize), so both finish.
        assert_eq!(state.finished_len(), 2);
        assert_eq!(state.pending_len(), 0);

        let (finished, _pending) = state.into_parts();
        // finished is (slot, QueryResult); slots preserve input order 0,1.
        let by_slot: std::collections::HashMap<usize, &QueryResult> =
            finished.iter().map(|(s, r)| (*s, r)).collect();

        let foo = by_slot[&0];
        assert_eq!(foo.row_count, 2, "two Foo instances matched");

        let bar = by_slot[&1];
        assert_eq!(bar.row_count, 1, "one Bar instance matched");
    }

    #[test]
    fn empty_driver_is_empty() {
        let driver: ScanDriver<'_, FakeResolver> = ScanDriver::new(Vec::new());
        assert!(driver.is_empty());
    }

    #[test]
    fn index_of_addr_default_is_none() {
        struct Bare;
        impl crate::query::execute::ClassResolver for Bare {
            fn class_name(&self, _c: u64) -> Option<&str> {
                None
            }
        }
        assert_eq!(Bare.index_of_addr(0x1000), None);
    }

    /// Resolver for RefWalk edge-capture tests: class 1 is "C" with a "parent"
    /// object field at offset 0 (ref width 8); addresses map to dense indices.
    struct RefFakeResolver {
        names: HashMap<u64, String>,
        addr_to_idx: HashMap<u64, usize>,
    }
    impl ClassResolver for RefFakeResolver {
        fn class_name(&self, class_id: u64) -> Option<&str> {
            self.names.get(&class_id).map(String::as_str)
        }
        fn field(&self, _class_id: u64, name: &str) -> Option<(u32, HprofType)> {
            if name == "parent" {
                Some((0, HprofType::Object))
            } else {
                None
            }
        }
        fn index_of_addr(&self, addr: u64) -> Option<usize> {
            self.addr_to_idx.get(&addr).copied()
        }
        fn ref_width(&self) -> usize {
            8
        }
    }

    fn be8(v: u64) -> [u8; 8] {
        v.to_be_bytes()
    }

    #[test]
    fn scan_driver_captures_refwalk_edges() {
        let resolver = RefFakeResolver {
            names: [(1u64, "C".to_string())].into_iter().collect(),
            addr_to_idx: [(0x100u64, 5usize), (0x200u64, 6usize)]
                .into_iter()
                .collect(),
        };
        let q = parse("SELECT x.parent.name FROM C x").unwrap();
        let p = pq(&q);
        assert!(p.needs.ref_walk, "query must arm ref_walk");

        let entries = vec![(0usize, SingleScanExecutor::new(&q, &p, &resolver))];
        let mut driver = ScanDriver::new(entries);

        driver.visit_instance(0, 1, &be8(0x100));
        driver.visit_instance(1, 1, &be8(0x200));

        let csr = driver.take_refwalk_csr(8);
        assert!(csr.is_some(), "armed driver yields a CSR");
        let (_off, tgt, fid) = csr.unwrap();
        assert_eq!(tgt, vec![5, 6]);
        assert_eq!(fid, vec![0, 0]);
    }

    #[test]
    fn scan_driver_null_ref_and_absent_field_and_unarmed() {
        // null ref (addr 0) → no edge; absent field → no edge (no panic).
        let resolver = RefFakeResolver {
            names: [(1u64, "C".to_string())].into_iter().collect(),
            addr_to_idx: [(0x100u64, 5usize)].into_iter().collect(),
        };
        let q = parse("SELECT x.parent.name FROM C x").unwrap();
        let p = pq(&q);
        let entries = vec![(0usize, SingleScanExecutor::new(&q, &p, &resolver))];
        let mut driver = ScanDriver::new(entries);
        driver.visit_instance(0, 1, &be8(0)); // null → no edge
        driver.visit_instance(1, 1, &be8(0x100)); // real → dense 5
        let (_off, tgt, _fid) = driver.take_refwalk_csr(8).unwrap();
        assert_eq!(tgt, vec![5], "only the non-null ref becomes an edge");

        // Unarmed (no RefWalk query) → take_refwalk_csr is None.
        let q2 = parse("SELECT @objectId FROM C").unwrap();
        let p2 = pq(&q2);
        let entries2 = vec![(0usize, SingleScanExecutor::new(&q2, &p2, &resolver))];
        let mut driver2 = ScanDriver::new(entries2);
        driver2.visit_instance(0, 1, &be8(0x100));
        assert!(driver2.take_refwalk_csr(8).is_none());
    }

    #[test]
    fn scan_driver_is_done_when_all_executors_satisfied() {
        let resolver = FakeResolver {
            names: [(10u64, "com.acme.Foo".to_string())].into_iter().collect(),
        };
        let q = parse("SELECT @objectId FROM com.acme.Foo LIMIT 2").unwrap();
        let plan = crate::query::optimize::optimize(
            pq(&q),
            &q,
            &crate::query::optimize::SchemaStats::default(),
        );
        assert_eq!(plan.scan_limit, Some(2), "optimizer must push LIMIT 2 down");
        let entries = vec![(0usize, SingleScanExecutor::new(&q, &plan, &resolver))];
        let mut driver = ScanDriver::new(entries);
        assert!(!driver.is_done(), "not done before any visits");
        driver.visit_instance(0, 10, &[]);
        assert!(!driver.is_done(), "not done after 1 of 2");
        driver.visit_instance(1, 10, &[]);
        assert!(driver.is_done(), "done after scan_limit reached");
        // A third visit must be skipped (executor already satisfied).
        driver.visit_instance(2, 10, &[]);
        let state = driver.finish_state();
        let (finished, _pending) = state.into_parts();
        let (_slot, res) = finished.into_iter().next().unwrap();
        assert_eq!(res.row_count, 2, "exactly 2 rows after early stop");
    }

    #[test]
    fn concat_union_appends_and_caps() {
        use crate::query::model::{QueryColumn, QueryValue};
        let col = || vec![QueryColumn { name: "c".into() }];
        let a = QueryResult {
            name: "q".into(),
            oql: "".into(),
            columns: col(),
            rows: vec![vec![QueryValue::Int(1)], vec![QueryValue::Int(2)]],
            row_count: 2,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let b = QueryResult {
            name: "q".into(),
            oql: "".into(),
            columns: col(),
            rows: vec![vec![QueryValue::Int(3)]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let out = concat_union(vec![a, b], 10);
        assert_eq!(out.row_count, 3);
        assert_eq!(out.rows.len(), 3);
        assert!(!out.truncated);
        assert_eq!(out.columns.len(), 1, "headers come from the head branch");

        let big = concat_union(
            vec![
                QueryResult {
                    name: "q".into(),
                    oql: "".into(),
                    columns: col(),
                    rows: (0..8).map(|i| vec![QueryValue::Int(i)]).collect(),
                    row_count: 8,
                    truncated: false,
                    error: None,
                    note: None,
                    viz: None,
                    elapsed_ms: None,
                },
                QueryResult {
                    name: "q".into(),
                    oql: "".into(),
                    columns: col(),
                    rows: (0..8).map(|i| vec![QueryValue::Int(i)]).collect(),
                    row_count: 8,
                    truncated: false,
                    error: None,
                    note: None,
                    viz: None,
                    elapsed_ms: None,
                },
            ],
            10,
        );
        assert_eq!(big.rows.len(), 10);
        assert!(big.truncated, "cap exceeded sets truncated");
    }

    #[test]
    fn concat_union_propagates_branch_truncation() {
        use crate::query::model::{QueryColumn, QueryValue};
        let col = || vec![QueryColumn { name: "c".into() }];
        let a = QueryResult {
            name: "q".into(),
            oql: "".into(),
            columns: col(),
            rows: vec![vec![QueryValue::Int(1)]],
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        // Second branch was itself truncated at scan time; UNION must carry that.
        let b = QueryResult {
            name: "q".into(),
            oql: "".into(),
            columns: col(),
            rows: vec![vec![QueryValue::Int(2)]],
            row_count: 1,
            truncated: true,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let out = concat_union(vec![a, b], 100);
        assert_eq!(out.rows.len(), 2);
        assert!(
            out.truncated,
            "a truncated branch taints the union even under cap"
        );
    }

    fn one_col_result(vals: &[i64]) -> QueryResult {
        use crate::query::model::{QueryColumn, QueryValue};
        QueryResult {
            name: String::new(),
            oql: String::new(),
            columns: vec![QueryColumn { name: "c".into() }],
            rows: vals.iter().map(|&v| vec![QueryValue::Int(v)]).collect(),
            row_count: vals.len() as u64,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        }
    }

    #[test]
    fn expand_union_flattens_head_then_branches() {
        // q0: plain; q1: 2 UNION branches (head + 2). Grouping must record
        // 1 slot for q0 and 3 consecutive slots for q1.
        let q_plain = parse("SELECT * FROM com.acme.Foo").unwrap();
        let p_plain = pq(&q_plain);
        let q_union = parse(
            "SELECT * FROM com.acme.Foo UNION SELECT * FROM com.acme.Bar UNION SELECT * FROM com.acme.Baz",
        )
        .unwrap();
        let p_union = pq(&q_union);

        let (flat, groups) = expand_union_queries(&[(q_plain, p_plain), (q_union, p_union)]);
        assert_eq!(flat.len(), 4, "1 plain + 3 union slots");
        // Every flat entry must carry no residual branch tail.
        for (q, p) in &flat {
            assert!(
                q.union_branches.is_empty(),
                "flattened AST keeps no branch tail"
            );
            assert!(
                p.union_branches.is_empty(),
                "flattened plan keeps no branch tail"
            );
        }
        assert_eq!(
            groups,
            vec![
                UnionGroup {
                    head: 0,
                    count: 1,
                    union_limit: None,
                    distinct: false,
                    limit: None,
                    offset: None,
                    intersect_count: 0,
                    except_count: 0,
                    order_by: vec![],
                },
                UnionGroup {
                    head: 1,
                    count: 3,
                    union_limit: None,
                    distinct: false,
                    limit: None,
                    offset: None,
                    intersect_count: 0,
                    except_count: 0,
                    order_by: vec![],
                },
            ]
        );
    }

    #[test]
    fn collapse_union_merges_branch_slots_only() {
        // Flat results for the layout above: slot0 plain (1 row), slots1-3 the
        // union branches (2 + 1 + 3 rows). After collapse: q0 untouched (1 row),
        // q1 concatenated (6 rows).
        let flat = vec![
            one_col_result(&[10]),
            one_col_result(&[1, 2]),
            one_col_result(&[3]),
            one_col_result(&[4, 5, 6]),
        ];
        let groups = vec![
            UnionGroup {
                head: 0,
                count: 1,
                union_limit: None,
                distinct: false,
                limit: None,
                offset: None,
                intersect_count: 0,
                except_count: 0,
                order_by: vec![],
            },
            UnionGroup {
                head: 1,
                count: 3,
                union_limit: None,
                distinct: false,
                limit: None,
                offset: None,
                intersect_count: 0,
                except_count: 0,
                order_by: vec![],
            },
        ];
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out.len(), 2, "one result per original query");
        assert_eq!(out[0].row_count, 1);
        assert_eq!(out[1].row_count, 6, "2 + 1 + 3 rows concatenated");
        assert!(!out[1].truncated);
    }

    // ---------- union-wide LIMIT (MAT gap #6) ----------

    #[test]
    fn collapse_applies_union_wide_limit_truncating() {
        // Two branches of 2 rows each = 4 total; union_limit 3 caps to 3 rows and
        // marks the result truncated (rows were dropped).
        let flat = vec![one_col_result(&[1, 2]), one_col_result(&[3, 4])];
        let groups = vec![UnionGroup {
            head: 0,
            count: 2,
            union_limit: Some(3),
            distinct: false,
            limit: None,
            offset: None,
            intersect_count: 0,
            except_count: 0,
            order_by: vec![],
        }];
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].rows.len(), 3, "capped to union_limit");
        assert_eq!(out[0].row_count, 3);
        assert!(
            out[0].truncated,
            "dropping rows for the union LIMIT truncates"
        );
    }

    #[test]
    fn collapse_union_limit_larger_than_total_returns_all() {
        // union_limit 99 exceeds the 3 total rows → all rows, not truncated.
        let flat = vec![one_col_result(&[1, 2]), one_col_result(&[3])];
        let groups = vec![UnionGroup {
            head: 0,
            count: 2,
            union_limit: Some(99),
            distinct: false,
            limit: None,
            offset: None,
            intersect_count: 0,
            except_count: 0,
            order_by: vec![],
        }];
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out[0].rows.len(), 3, "all rows returned");
        assert!(!out[0].truncated, "no rows dropped → not truncated");
    }

    #[test]
    fn collapse_union_limit_zero_returns_no_rows() {
        // union_limit 0 → zero rows even though branches have rows; truncated.
        let flat = vec![one_col_result(&[1, 2]), one_col_result(&[3, 4])];
        let groups = vec![UnionGroup {
            head: 0,
            count: 2,
            union_limit: Some(0),
            distinct: false,
            limit: None,
            offset: None,
            intersect_count: 0,
            except_count: 0,
            order_by: vec![],
        }];
        let out = collapse_union_results(flat, &groups);
        assert!(out[0].rows.is_empty(), "LIMIT 0 → no rows");
        assert_eq!(out[0].row_count, 0);
        assert!(out[0].truncated);
    }

    #[test]
    fn collapse_union_limit_none_uses_overall_cap_only() {
        // No union_limit → old behavior: all rows kept (well under OVERALL cap).
        let flat = vec![one_col_result(&[1, 2]), one_col_result(&[3, 4, 5])];
        let groups = vec![UnionGroup {
            head: 0,
            count: 2,
            union_limit: None,
            distinct: false,
            limit: None,
            offset: None,
            intersect_count: 0,
            except_count: 0,
            order_by: vec![],
        }];
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out[0].rows.len(), 5);
        assert!(!out[0].truncated);
    }

    #[test]
    fn collapse_union_limit_equal_to_total_not_truncated() {
        // union_limit exactly equal to the total row count returns all rows and
        // must NOT be marked truncated (nothing was dropped).
        let flat = vec![one_col_result(&[1, 2]), one_col_result(&[3])];
        let groups = vec![UnionGroup {
            head: 0,
            count: 2,
            union_limit: Some(3),
            distinct: false,
            limit: None,
            offset: None,
            intersect_count: 0,
            except_count: 0,
            order_by: vec![],
        }];
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out[0].rows.len(), 3);
        assert!(!out[0].truncated, "exact fit is not a truncation");
    }

    #[test]
    fn concat_union_caps_head_rows_too() {
        // A cap smaller than the head branch's own row count must truncate the
        // head, not just the appended branches.
        let out = concat_union(vec![one_col_result(&[1, 2, 3, 4]), one_col_result(&[5])], 2);
        assert_eq!(out.rows.len(), 2, "head alone exceeds the cap → truncated");
        assert!(out.truncated);
    }

    #[test]
    fn expand_union_queries_propagates_union_limit_to_group() {
        // A parsed+planned union with a trailing LIMIT must carry that union_limit
        // onto its UnionGroup so collapse can apply it.
        let q = parse(
            "SELECT @objectId FROM java.lang.String \
             UNION (SELECT @objectId FROM java.lang.Object) LIMIT 7",
        )
        .unwrap();
        assert_eq!(q.union_limit, Some(7), "parser sets union_limit");
        let p = pq(&q);
        assert_eq!(p.union_limit, Some(7), "planner propagates union_limit");
        let (_flat, groups) = expand_union_queries(&[(q, p)]);
        assert_eq!(groups.len(), 1);
        assert_eq!(
            groups[0].union_limit,
            Some(7),
            "UnionGroup carries the union-wide LIMIT"
        );
    }

    #[test]
    fn expand_collapse_roundtrip_preserves_plain_query_order() {
        // Two plain queries: flatten is a no-op grouping and collapse returns
        // them in the same order with contents intact.
        let flat = vec![one_col_result(&[1]), one_col_result(&[2, 3])];
        let groups = vec![
            UnionGroup {
                head: 0,
                count: 1,
                union_limit: None,
                distinct: false,
                limit: None,
                offset: None,
                intersect_count: 0,
                except_count: 0,
                order_by: vec![],
            },
            UnionGroup {
                head: 1,
                count: 1,
                union_limit: None,
                distinct: false,
                limit: None,
                offset: None,
                intersect_count: 0,
                except_count: 0,
                order_by: vec![],
            },
        ];
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].row_count, 1);
        assert_eq!(out[1].row_count, 2);
    }

    // ---------- SELECT DISTINCT dedup (Task 4) ----------

    fn distinct_group(rows: &[i64]) -> (Vec<QueryResult>, Vec<UnionGroup>) {
        (
            vec![one_col_result(rows)],
            vec![UnionGroup {
                head: 0,
                count: 1,
                union_limit: None,
                distinct: true,
                limit: None,
                offset: None,
                intersect_count: 0,
                except_count: 0,
                order_by: vec![],
            }],
        )
    }

    #[test]
    fn distinct_single_group_deduplicates_rows() {
        // Rows [1,2,1,3,2] → deduped to [1,2,3] (stable first-occurrence order).
        let (flat, groups) = distinct_group(&[1, 2, 1, 3, 2]);
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        use crate::query::model::QueryValue;
        let vals: Vec<i64> = r
            .rows
            .iter()
            .map(|row| match row[0] {
                QueryValue::Int(v) => v,
                _ => panic!("unexpected"),
            })
            .collect();
        assert_eq!(vals, vec![1, 2, 3], "first-occurrence stable dedup");
        assert_eq!(r.row_count, 3);
    }

    #[test]
    fn distinct_single_group_all_unique_unchanged() {
        // All unique: dedup is a no-op.
        let (flat, groups) = distinct_group(&[5, 3, 1]);
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out[0].row_count, 3);
    }

    #[test]
    fn distinct_single_group_all_same_returns_one() {
        let (flat, groups) = distinct_group(&[7, 7, 7, 7]);
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out[0].row_count, 1);
    }

    #[test]
    fn distinct_union_group_removes_cross_branch_dupes() {
        // Two branches both producing [1,2,3]: DISTINCT must yield [1,2,3], not [1,2,3,1,2,3].
        use crate::query::model::{QueryColumn, QueryValue};
        let col = || vec![QueryColumn { name: "v".into() }];
        let branch = |vals: &[i64]| QueryResult {
            name: String::new(),
            oql: String::new(),
            columns: col(),
            rows: vals.iter().map(|&v| vec![QueryValue::Int(v)]).collect(),
            row_count: vals.len() as u64,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let flat = vec![branch(&[1, 2, 3]), branch(&[2, 3, 4])];
        let groups = vec![UnionGroup {
            head: 0,
            count: 2,
            union_limit: None,
            distinct: true,
            limit: None,
            offset: None,
            intersect_count: 0,
            except_count: 0,
            order_by: vec![],
        }];
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out[0].row_count, 4, "cross-branch dupes removed: 1,2,3,4");
        let vals: Vec<i64> = out[0]
            .rows
            .iter()
            .map(|row| match row[0] {
                QueryValue::Int(v) => v,
                _ => panic!(),
            })
            .collect();
        assert_eq!(vals, vec![1, 2, 3, 4]);
    }

    #[test]
    fn distinct_float_column_deduplicates_without_panic() {
        // Float rows with equal values are deduped; NaN must not panic.
        use crate::query::model::{QueryColumn, QueryValue};
        let col = || vec![QueryColumn { name: "f".into() }];
        let flat = vec![QueryResult {
            name: String::new(),
            oql: String::new(),
            columns: col(),
            rows: vec![
                vec![QueryValue::Float(1.5)],
                vec![QueryValue::Float(1.5)],
                vec![QueryValue::Float(f64::NAN)],
                vec![QueryValue::Float(f64::NAN)],
                vec![QueryValue::Float(2.0)],
            ],
            row_count: 5,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        }];
        let groups = vec![UnionGroup {
            head: 0,
            count: 1,
            union_limit: None,
            distinct: true,
            limit: None,
            offset: None,
            intersect_count: 0,
            except_count: 0,
            order_by: vec![],
        }];
        let out = collapse_union_results(flat, &groups);
        // Two NaN rows should also dedup (Debug format is total: "Float(NaN)" == "Float(NaN)").
        assert_eq!(out[0].row_count, 3, "1.5, NaN, 2.0 remain after dedup");
    }

    #[test]
    fn non_distinct_group_with_dupes_is_not_deduped() {
        // Non-DISTINCT invariant: duplicate rows must pass through unchanged.
        let (flat, mut groups) = distinct_group(&[1, 1, 2]);
        groups[0].distinct = false;
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out[0].row_count, 3, "non-distinct must preserve duplicates");
    }

    #[test]
    fn distinct_with_limit_applies_limit_after_dedup() {
        // 5 rows where [1,2,1,3,2,4,5] → distinct [1,2,3,4,5], then LIMIT 3 → [1,2,3].
        let (flat, mut groups) = distinct_group(&[1, 2, 1, 3, 2, 4, 5]);
        groups[0].limit = Some(3);
        let out = collapse_union_results(flat, &groups);
        use crate::query::model::QueryValue;
        let vals: Vec<i64> = out[0]
            .rows
            .iter()
            .map(|row| match row[0] {
                QueryValue::Int(v) => v,
                _ => panic!(),
            })
            .collect();
        assert_eq!(vals, vec![1, 2, 3], "LIMIT applied after dedup");
        assert_eq!(out[0].row_count, 3);
        assert!(out[0].truncated, "truncated because limit dropped rows");
    }

    #[test]
    fn distinct_with_limit_ge_distinct_count_returns_all() {
        // LIMIT 10 on 3 distinct rows → all 3, not truncated.
        let (flat, mut groups) = distinct_group(&[1, 2, 1, 3]);
        groups[0].limit = Some(10);
        let out = collapse_union_results(flat, &groups);
        assert_eq!(out[0].row_count, 3);
        assert!(!out[0].truncated, "limit not reached → not truncated");
    }

    #[test]
    fn expand_union_propagates_distinct_and_limit_to_group() {
        // A DISTINCT query with LIMIT must set both fields on its UnionGroup.
        let q = parse("SELECT DISTINCT @objectId FROM java.lang.String LIMIT 5").unwrap();
        assert!(q.distinct, "parser must set distinct");
        assert_eq!(q.limit, Some(5));
        let p = pq(&q);
        // Scan-time limit is cleared for DISTINCT (deferred to collapse).
        assert_eq!(p.limit, None, "scan-time limit cleared for DISTINCT");
        let (_flat, groups) = expand_union_queries(&[(q, p)]);
        assert_eq!(groups.len(), 1);
        assert!(groups[0].distinct, "UnionGroup.distinct must be true");
        assert_eq!(
            groups[0].limit,
            Some(5),
            "UnionGroup.limit carries the deferred limit"
        );
    }

    #[test]
    fn expand_union_non_distinct_limit_not_deferred() {
        // A non-DISTINCT query with LIMIT must NOT set the distinct flag.
        let q = parse("SELECT @objectId FROM java.lang.String LIMIT 5").unwrap();
        assert!(!q.distinct);
        let p = pq(&q);
        assert_eq!(p.limit, Some(5), "non-distinct keeps scan-time limit");
        let (_flat, groups) = expand_union_queries(&[(q, p)]);
        assert!(!groups[0].distinct);
        assert_eq!(groups[0].limit, Some(5));
        // limit is populated from q.limit but only READ for distinct groups; this asserts population, not use.
    }

    // ---------- subquery helpers (Task 23) ----------

    #[test]
    fn intersect_from_subquery_semijoin() {
        // outer scan produced dense idx [1,2,3,5]; inner produced [2,3,4]
        let (kept, trunc) = intersect_from_subquery(&[2, 3, 4], false, &[1, 2, 3, 5]);
        assert_eq!(kept, vec![2, 3]);
        assert!(!trunc);
    }

    #[test]
    fn intersect_from_subquery_propagates_truncation() {
        let (_k, trunc) = intersect_from_subquery(&[2, 3], true, &[2, 3]);
        assert!(
            trunc,
            "inner truncation must propagate — result is incomplete"
        );
    }

    #[test]
    fn intersect_from_subquery_disjoint_is_empty() {
        let (kept, _t) = intersect_from_subquery(&[10, 11], false, &[1, 2, 3]);
        assert!(kept.is_empty());
    }

    #[test]
    fn in_subquery_set_membership() {
        let (set, trunc) = build_in_subquery_set(&[10, 20, 30], 100);
        assert!(!trunc);
        assert!(in_subquery_contains(&set, 20));
        assert!(!in_subquery_contains(&set, 99));
        let (_s, t) = build_in_subquery_set(&[1, 2, 3, 4], 2);
        assert!(t, "cap exceeded sets truncated");
    }

    #[test]
    fn in_subquery_set_dedups() {
        // Duplicate addresses collapse; membership unaffected, cap counts uniques.
        let (set, trunc) = build_in_subquery_set(&[5, 5, 5], 100);
        assert!(!trunc);
        assert_eq!(set.len(), 1);
        assert!(in_subquery_contains(&set, 5));
    }

    #[test]
    fn reachability_filter_drops_unreachable_rows() {
        use crate::query::model::QueryValue;
        // dfn: idx 0 reachable (0), idx 1 unreachable (u32::MAX), idx 2 reachable (5)
        let dfn = vec![0u32, u32::MAX, 5];
        let mut result = QueryResult {
            name: "t".into(),
            oql: String::new(),
            columns: vec![],
            rows: vec![
                vec![QueryValue::ObjRef {
                    index: 0,
                    class: "C".into(),
                    addr: None,
                }],
                vec![QueryValue::ObjRef {
                    index: 1,
                    class: "C".into(),
                    addr: None,
                }],
                vec![QueryValue::ObjRef {
                    index: 2,
                    class: "C".into(),
                    addr: None,
                }],
                vec![QueryValue::Int(99)],
            ],
            row_count: 4,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        // Explicit per-row source dense indices captured at scan time. The row
        // VALUES are irrelevant now — pruning is by the captured src, not by
        // sniffing the projected value.
        let src = vec![0u32, 1, 2, 99];
        filter_result_by_src(&mut result, &src, &dfn);
        // src[1]=1 dropped (unreachable); src[3]=99 dropped (out-of-range);
        // src[0]=0 and src[2]=2 kept.
        assert_eq!(
            result.row_count, 2,
            "unreachable + out-of-range src dropped"
        );
        assert_eq!(result.rows.len(), 2);
    }

    /// `@objectAddress` projects the raw heap address as `Int`, which the OLD
    /// value-sniffing prune mis-read as a dense index and dropped. With the
    /// scan-captured source index, the row is kept iff its SOURCE object (not the
    /// projected address) is reachable — even for a huge address value.
    #[test]
    fn reachability_filter_keeps_object_address_rows_by_source() {
        use crate::query::model::QueryValue;
        let dfn = vec![0u32, u32::MAX]; // idx 0 reachable, idx 1 unreachable
        let mut result = QueryResult {
            name: "t".into(),
            oql: String::new(),
            columns: vec![],
            // Projected values are big heap addresses, NOT dense indices.
            rows: vec![
                vec![QueryValue::Int(0x7f00_1234_5678)], // src 0 → reachable → KEPT
                vec![QueryValue::Int(0x7f00_9abc_def0)], // src 1 → unreachable → DROPPED
            ],
            row_count: 2,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        let src = vec![0u32, 1];
        filter_result_by_src(&mut result, &src, &dfn);
        assert_eq!(result.rows.len(), 1, "reachable @objectAddress row kept");
        assert_eq!(result.rows[0][0], QueryValue::Int(0x7f00_1234_5678));
    }

    /// A slot with NO captured src (aggregate `COUNT(*)`, scalar, error) is never
    /// pruned: the resume layer simply skips `filter_result_by_src` for it, so
    /// the single aggregate row survives regardless of reachability.
    #[test]
    fn reachability_filter_never_prunes_when_no_src_captured() {
        use crate::query::model::QueryValue;
        let dfn = vec![u32::MAX, u32::MAX]; // nothing reachable
        let mut result = QueryResult {
            name: "count".into(),
            oql: String::new(),
            columns: vec![],
            rows: vec![vec![QueryValue::Int(42)]], // COUNT(*) = 42
            row_count: 1,
            truncated: false,
            error: None,
            note: None,
            viz: None,
            elapsed_ms: None,
        };
        // Empty src (no captured source object) → keep all rows.
        filter_result_by_src(&mut result, &[], &dfn);
        assert_eq!(result.rows.len(), 1, "aggregate row is never pruned");
        assert_eq!(result.rows[0][0], QueryValue::Int(42));
    }
}
