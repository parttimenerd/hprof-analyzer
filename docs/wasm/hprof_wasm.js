/* @ts-self-types="./hprof_wasm.d.ts" */

/**
 * An active hprof analysis session backed by in-memory bytes.
 *
 * Call `HprofSession.load(array, name)` from JS to initialise a session.
 * Once loaded, use `query()` for OQL queries and `run_full_analysis()` to
 * pre-compute retained sizes and generate the HTML report.
 */
export class HprofSession {
    static __wrap(ptr) {
        const obj = Object.create(HprofSession.prototype);
        obj.__wbg_ptr = ptr;
        HprofSessionFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        HprofSessionFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_hprofsession_free(ptr, 0);
    }
    /**
     * Returns up to `max_paths` distinct shortest paths from `dense_idx` to GC roots.
     *
     * Uses multi-source BFS from the target; each time a GC root is reached the path
     * is recorded. Paths sharing a prefix are NOT merged — each is a complete chain.
     * Cap `max_paths` at 10.
     *
     * Returns `{"ok":true,"paths":[{"path":[...],"root_type":"..."},...],"total_found":N}`.
     * Each path is root→target order. Each node: `{"dense_idx":N,"display_class":"...","shallow":N,"retained":N}`.
     * @param {number} dense_idx
     * @param {number} max_paths
     * @returns {string}
     */
    all_gc_root_paths(dense_idx, max_paths) {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_all_gc_root_paths(this.__wbg_ptr, dense_idx, max_paths);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Returns a JSON array of class names extracted from the loaded dump.
     * @returns {string}
     */
    class_names() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_class_names(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * OQL tab-completion using the loaded session's class and field data.
     *
     * Returns the same JSON array format as the free `complete()` function but
     * uses the session's `ClassFieldIndex` so `alias.field` completions work.
     * @param {string} line
     * @param {number} cursor_pos
     * @returns {string}
     */
    complete_query(line, cursor_pos) {
        let deferred2_0;
        let deferred2_1;
        try {
            const ptr0 = passStringToWasm0(line, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ret = wasm.hprofsession_complete_query(this.__wbg_ptr, ptr0, len0, cursor_pos);
            deferred2_0 = ret[0];
            deferred2_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Build and cache the inbound CSR for interactive exploration (BFS queries).
     * Must be called before `inbound_refs()` or `gc_root_path()`.
     * If `run_full_analysis()` has been called, retained sizes are included.
     */
    enable_exploration() {
        const ret = wasm.hprofsession_enable_exploration(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * @param {bigint} addr
     * @returns {string}
     */
    find_dense_by_address(addr) {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_find_dense_by_address(this.__wbg_ptr, addr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Find all captured objects whose class name contains `class_prefix` (case-insensitive).
     *
     * Returns `{"ok":true,"matches":[...],"total":N,"truncated":bool}`.
     * Each match: `{"dense_idx":N,"display_class":"...","shallow":N,"retained":N}`.
     * Results are sorted by retained heap descending.
     * Requires `enable_exploration()` to have been called first.
     * @param {string} class_prefix
     * @param {number} limit
     * @returns {string}
     */
    find_instances(class_prefix, limit) {
        let deferred2_0;
        let deferred2_1;
        try {
            const ptr0 = passStringToWasm0(class_prefix, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ret = wasm.hprofsession_find_instances(this.__wbg_ptr, ptr0, len0, limit);
            deferred2_0 = ret[0];
            deferred2_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * @param {number} src_idx
     * @param {number} dst_idx
     * @returns {string}
     */
    find_path_between(src_idx, dst_idx) {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_find_path_between(this.__wbg_ptr, src_idx, dst_idx);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Scan for sensitive data in heap strings using built-in patterns.
     *
     * Builds the string-values map once (single file scan), then applies all
     * patterns in-memory — much faster than one `query()` call per pattern.
     *
     * Returns JSON:
     * ```json
     * {"ok":true,"findings":[{"category":"...","value":"..."},...]}
     * ```
     * or `{"ok":false,"error":{"message":"..."}}` on failure.
     * @returns {string}
     */
    find_secrets() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_find_secrets(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * BFS from `dense_idx` to the nearest GC root through inbound edges.
     *
     * Returns `{"ok":true,"path":[...],"root_type":"..."}` on success,
     * `{"ok":false,"error":"no_path"}` if no path was found,
     * or `{"error":"exploration_not_enabled"}` if `enable_exploration()` was not called.
     *
     * Path is ordered root → target. Each node: `{"dense_idx":N,"display_class":"...","shallow":N,"retained":N,"field_name":""}`
     * @param {number} dense_idx
     * @returns {string}
     */
    gc_root_path(dense_idx) {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_gc_root_path(this.__wbg_ptr, dense_idx);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Run the full analysis pipeline and return the report as a JSON string.
     * @returns {string}
     */
    generate_report() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.hprofsession_generate_report(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Run the full analysis pipeline and return a self-contained HTML document.
     * @returns {string}
     */
    generate_report_html() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.hprofsession_generate_report_html(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Returns key-value or element entries for known Java collection types.
     *
     * Recognises: HashMap, LinkedHashMap, ConcurrentHashMap, HashSet, LinkedHashSet,
     * TreeMap, ArrayList, LinkedList, ArrayDeque, Vector, Stack, and several Scala/
     * Kotlin/Eclipse Collections/Guava types.
     *
     * Strategy:
     * 1. Identify the collection kind + backing-array field via `collection_info`.
     * 2. Run `SELECT * FROM ClassName WHERE @objectId = N` to get the field values,
     *    locate the backing array field, and retrieve its dense index.
     * 3. Call `outbound_refs` on the array to enumerate entries.
     *
     * Returns `{"ok":true,"type":"map","entries":[{"key_idx":N,"key_class":"...","val_idx":M,"val_class":"..."},...],"truncated":bool}`
     * for map-like types, or `{"ok":true,"type":"list","entries":[{"elem_idx":N,"elem_class":"..."},...],"truncated":bool}`
     * for list/set types.
     * Returns `{"ok":true,"type":"unknown"}` when the class is not a known collection.
     * @param {number} dense_idx
     * @param {number} limit
     * @returns {string}
     */
    get_collection_entries(dense_idx, limit) {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_get_collection_entries(this.__wbg_ptr, dense_idx, limit);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Returns primitive and reference field values for a single object.
     *
     * Uses the OQL engine to run `SELECT * FROM ClassName s WHERE s.@objectId = N`
     * and maps the result columns to typed field entries.
     *
     * Returns `{"ok":true,"fields":[{"name":"size","kind":"int","value":47823},{"name":"table","kind":"ref","display_class":"Entry[]","dense_idx":1234},...]}`.
     * On failure: `{"ok":false,"error":"..."}`.
     * Requires the OQL cache to have been built (call `query()` at least once, or `run_full_analysis()`).
     * @param {number} dense_idx
     * @returns {string}
     */
    get_field_values(dense_idx) {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_get_field_values(this.__wbg_ptr, dense_idx);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Returns class name, shallow, and retained for a single object by dense index.
     *
     * Returns `{"ok":true,"display_class":"...","shallow":N,"retained":N}` on success,
     * or `{"error":"exploration_not_enabled"}` / `{"error":"out_of_range"}`.
     * Requires `enable_exploration()` to have been called first.
     * @param {number} dense_idx
     * @returns {string}
     */
    get_node_info(dense_idx) {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_get_node_info(this.__wbg_ptr, dense_idx);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Returns the HPROF memory address (as a hex string) for a single object by dense index.
     *
     * Returns `{"ok":true,"address":"0x..."}` on success,
     * or `{"error":"exploration_not_enabled"}` / `{"error":"out_of_range"}` / `{"error":"no_addresses"}`.
     * Requires `enable_exploration()` to have been called first.
     * @param {number} dense_idx
     * @returns {string}
     */
    get_object_address(dense_idx) {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_get_object_address(this.__wbg_ptr, dense_idx);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Return the cached HTML report generated during `run_full_analysis()`.
     * Returns an empty string if `run_full_analysis()` has not been called.
     * @returns {string}
     */
    get_report_html() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_get_report_html(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Returns `true` if `run_full_analysis()` has been called.
     * @returns {boolean}
     */
    has_retained() {
        const ret = wasm.hprofsession_has_retained(this.__wbg_ptr);
        return ret !== 0;
    }
    /**
     * Returns a JSON object listing inbound referrers of the given dense object index.
     *
     * Returns `{"ok":true,"refs":[...],"total":N,"truncated":bool}` on success,
     * or `{"error":"exploration_not_enabled"}` if `enable_exploration()` was not called.
     *
     * Each ref entry: `{"src_idx":N,"field_name":"","display_class":"...","shallow":N,"retained":N}`
     * @param {number} dense_idx
     * @param {number} limit
     * @returns {string}
     */
    inbound_refs(dense_idx, limit) {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_inbound_refs(this.__wbg_ptr, dense_idx, limit);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Load a `.hprof` file from a JS `Uint8Array` and build the query cache.
     *
     * # Memory strategy
     *
     * wasm-bindgen moves the JS `Uint8Array` into WASM linear memory once, as
     * an owned `Vec<u8>` parameter — no extra copy on our side.  From that
     * point everything happens inside WASM:
     *
     * 1. We take **ownership** of that buffer as `Arc<Vec<u8>>` (Vec is Sized,
     *    so `Arc::try_unwrap` works on it after parsing completes).
     * 2. The Arc is cloned cheaply for `parse_source` (Pass1 × 2 + Pass2).
     * 3. After parsing, both external Arc clones are dropped (refcount → 1),
     *    then `Arc::try_unwrap` reclaims the `Vec<u8>` with **zero copy**.
     * 4. Gzip-compress in 256 KB chunks; `drop(raw)` before `enc.finish()` so
     *    the raw buffer (N bytes) is freed before the compressed tail is written.
     *    Raw and compressed never coexist simultaneously.
     *
     * Taking `data` by owned `Vec<u8>` (not `&[u8]`) is deliberate: wasm-bindgen
     * hands us ownership of the buffer it already placed in linear memory, so
     * there is no transient second full-file copy. That roughly halves the
     * tightest-moment footprint for a plain (non-gzip) `.hprof`, raising the
     * largest file that fits under the wasm32 4 GiB linear-memory cap.
     *
     * Peak WASM footprint = N (parse buffer) + analysis structures.
     * After load() returns only the compressed copy survives (~N/4).
     * @param {Uint8Array} data
     * @param {string} name
     * @returns {HprofSession}
     */
    static load(data, name) {
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.hprofsession_load(ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return HprofSession.__wrap(ret[0]);
    }
    /**
     * Like `load()` but fires a JS progress callback at phase boundaries.
     *
     * The callback receives `(phase: string, fraction: number)` where fraction
     * is always 1.0 (phase complete).  Phases fired in order:
     *   "compress", "pass1_a", "pass1_b", "pass2"
     *
     * Because WASM is single-threaded, the browser will not repaint between
     * callbacks — but each call allows JS to update DOM state that is rendered
     * after the full load() returns control to the event loop.
     * @param {Uint8Array} data
     * @param {string} name
     * @param {Function} cb
     * @returns {HprofSession}
     */
    static load_with_progress(data, name, cb) {
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.hprofsession_load_with_progress(ptr0, len0, ptr1, len1, cb);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return HprofSession.__wrap(ret[0]);
    }
    /**
     * Returns a JSON array of all built-in named queries.
     * @returns {string}
     */
    static named_queries() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_named_queries();
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Returns the OQL language reference as a JSON object with keys:
     * keywords, reserved, aggregates, functions, methods, attributes.
     * Same structure as the server's GET /help endpoint (minus dump-specific
     * class/field lists) so the WASM shell can show /help oql offline.
     * @returns {string}
     */
    static oql_help() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_oql_help();
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Returns a JSON object listing outbound references from the given dense object index.
     *
     * Returns `{"ok":true,"refs":[...],"total":N,"truncated":bool}` on success,
     * or `{"error":"exploration_not_enabled"}` if `enable_exploration()` was not called.
     *
     * Each ref entry: `{"dst_idx":N,"field_name":"...","display_class":"...","shallow":N,"retained":N}`
     * Field names are included when the dump was analyzed with `--ref-paths`; otherwise empty strings.
     * @param {number} dense_idx
     * @param {number} limit
     * @returns {string}
     */
    outbound_refs(dense_idx, limit) {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_outbound_refs(this.__wbg_ptr, dense_idx, limit);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Run an OQL query and return a JSON string.
     *
     * Success: `{"ok":true,"result":{"columns":[...],"rows":[...],"row_count":N}}`
     * Error:   `{"ok":false,"error":{"message":"..."}}`
     * @param {string} oql
     * @returns {string}
     */
    query(oql) {
        let deferred2_0;
        let deferred2_1;
        try {
            const ptr0 = passStringToWasm0(oql, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ret = wasm.hprofsession_query(this.__wbg_ptr, ptr0, len0);
            deferred2_0 = ret[0];
            deferred2_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Redact a heap dump in memory and return the redacted raw `.hprof` bytes.
     *
     * `mode` selects the redaction depth:
     *   - `"lean"` (default): single-pass, zeros only primitive array elements
     *     (byte[], char[], short[], int[], long[], float[], double[], boolean[]).
     *     Instance scalar fields are left untouched.
     *   - `"complete"`: two-pass, also zeros primitive scalar fields on instances
     *     and static fields on classes.
     *
     * `cb(phase: string, fraction: number)` is called for progress updates.
     * @param {Uint8Array} data
     * @param {string} name
     * @param {string} mode
     * @param {Function} cb
     * @returns {Uint8Array}
     */
    static redact_with_progress(data, name, mode, cb) {
        const ptr0 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(mode, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.hprofsession_redact_with_progress(ptr0, len0, ptr1, len1, ptr2, len2, cb);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v4 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v4;
    }
    /**
     * Pre-compute dominators + retained sizes so `@retainedHeapSize` queries
     * are served from the cached array on subsequent `query()` calls.
     * Also generates and caches the HTML report for instant `get_report_html()`.
     */
    run_full_analysis() {
        const ret = wasm.hprofsession_run_full_analysis(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Like `run_full_analysis_with_progress()` but with optional extended passes.
     *
     * `find_duplicates` — enable duplicate string/array detection.
     * `collections`     — enable collection fill-ratio and waste analysis.
     * @param {boolean} find_duplicates
     * @param {boolean} collections
     * @param {Function} cb
     */
    run_full_analysis_with_options_and_progress(find_duplicates, collections, cb) {
        const ret = wasm.hprofsession_run_full_analysis_with_options_and_progress(this.__wbg_ptr, find_duplicates, collections, cb);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Like `run_full_analysis()` but fires a JS progress callback at each phase.
     *
     * Callback receives `(phase: string, fraction: number)`.  Phases in order:
     *   "pass1", "pass2", "rpo", "inbound", "dominators", "retained"
     * @param {Function} cb
     */
    run_full_analysis_with_progress(cb) {
        const ret = wasm.hprofsession_run_full_analysis_with_progress(this.__wbg_ptr, cb);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Returns a JSON object with heap statistics available immediately after `load()`:
     * `{ instance_count: N, class_count: N, compressed_bytes: N }`
     *
     * `instance_count` is the exact object count from Pass1 — use it to display
     * "Computing dominators for N objects" and to estimate the dominator phase duration.
     * @returns {string}
     */
    stats() {
        let deferred1_0;
        let deferred1_1;
        try {
            const ret = wasm.hprofsession_stats(this.__wbg_ptr);
            deferred1_0 = ret[0];
            deferred1_1 = ret[1];
            return getStringFromWasm0(ret[0], ret[1]);
        } finally {
            wasm.__wbindgen_free(deferred1_0, deferred1_1, 1);
        }
    }
    /**
     * Returns the compressed size of the stored HPROF bytes (bytes).
     * @returns {number}
     */
    stored_bytes() {
        const ret = wasm.hprofsession_stored_bytes(this.__wbg_ptr);
        return ret >>> 0;
    }
}
if (Symbol.dispose) HprofSession.prototype[Symbol.dispose] = HprofSession.prototype.free;

/**
 * OQL tab-completion suggestions for a partial input line.
 *
 * Returns a JSON array: `[{"value":"...","display":"...","group":"..."},...]`
 *
 * This free function has no access to the loaded session's field data.
 * Use `HprofSession.complete_query()` instead when a session is loaded.
 * @param {string} line
 * @param {number} cursor_pos
 * @param {string[]} class_names
 * @returns {string}
 */
export function complete(line, cursor_pos, class_names) {
    let deferred3_0;
    let deferred3_1;
    try {
        const ptr0 = passStringToWasm0(line, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayJsValueToWasm0(class_names, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.complete(ptr0, len0, cursor_pos, ptr1, len1);
        deferred3_0 = ret[0];
        deferred3_1 = ret[1];
        return getStringFromWasm0(ret[0], ret[1]);
    } finally {
        wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
    }
}

export function init() {
    wasm.init();
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_string_get_d154f1e671052120: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_bb96b2010945f0bc: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg_call_0f2a9af232c18fd2: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = arg0.call(arg1, arg2, arg3);
            return ret;
        }, arguments); },
        __wbg_error_757e9472f8410341: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return ret;
        },
        __wbg_stack_3b0d974bbf31e44f: function(arg0, arg1) {
            const ret = arg1.stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbindgen_cast_0000000000000001: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./hprof_wasm_bg.js": import0,
    };
}

const HprofSessionFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_hprofsession_free(ptr, 1));

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayJsValueToWasm0(array, malloc) {
    const ptr = malloc(array.length * 4, 4) >>> 0;
    for (let i = 0; i < array.length; i++) {
        const add = addToExternrefTable0(array[i]);
        getDataViewMemory0().setUint32(ptr + 4 * i, add, true);
    }
    WASM_VECTOR_LEN = array.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (!module.ok) {
            throw new Error(`failed to fetch Wasm: ${module.status} ${module.statusText} fetching '${module.url}'`);
        }

        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('hprof_wasm_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
