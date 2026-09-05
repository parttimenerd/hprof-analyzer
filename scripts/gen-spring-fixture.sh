#!/usr/bin/env bash
# scripts/gen-spring-fixture.sh
#
# Generates docs/samples/spring-petclinic-h2-ai.hprof.gz — a Spring PetClinic
# heap dump that contains detectable secrets:
#   - JDBC URL with embedded password (petclinic123)
#   - Fake OpenAI-style API key (sk-demo-...)
#
# How the secrets land on the heap:
#   Spring loads application.properties into its Environment, which keeps all
#   property values as java.lang.String objects. No Spring AI code is needed —
#   the key just needs to be a property value that Spring reads on startup.
#
# Prerequisites:
#   - Java 17+  (java on PATH)
#   - Maven 3.8+  (mvn on PATH, or ./mvnw in the cloned repo)
#   - curl, jmap
#
# Usage:
#   bash scripts/gen-spring-fixture.sh

set -euo pipefail
PROJ_ROOT="$(git rev-parse --show-toplevel)"
cd "$PROJ_ROOT"

FIXTURE_OUT="$PROJ_ROOT/docs/samples/spring-petclinic-h2-ai.hprof.gz"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

echo "=== Spring PetClinic + H2 fixture generator ==="
echo "Output: $FIXTURE_OUT"
echo ""

# ── Step 1: Clone Spring PetClinic ───────────────────────────────────────────
PETCLINIC_DIR=$TMP_DIR/spring-petclinic
echo "1. Cloning Spring PetClinic..."
git clone --depth=1 https://github.com/spring-projects/spring-petclinic "$PETCLINIC_DIR"
echo "   Cloned."

# ── Step 2: Inject demo secrets via application.properties ──────────────────
# These land in the Spring Environment as java.lang.String values on the heap.
# We override the existing H2 datasource URL to embed a password, and add a
# fake API key property that looks like a real OpenAI token.
echo "2. Injecting demo secrets into application.properties..."
cat >> "$PETCLINIC_DIR/src/main/resources/application.properties" <<'EOF'

# ── Demo secrets for heap dump fixture (educational purposes only) ────────────
# Override datasource URL to embed password — Spring keeps this as a String
spring.datasource.url=jdbc:h2:mem:petclinic;DB_CLOSE_DELAY=-1;password=petclinic123
spring.datasource.username=sa
spring.datasource.password=petclinic123

# Fake AI API key — stored in Spring Environment as a plain String property
demo.ai.api-key=sk-demo-thisisasecret12345678901234
demo.ai.base-url=http://localhost:11434/v1
demo.ai.model=gpt-4o
EOF

# ── Step 3: Build ────────────────────────────────────────────────────────────
echo "3. Building Spring PetClinic (skipping tests)..."
cd "$PETCLINIC_DIR"
./mvnw -q package -DskipTests 2>&1 | grep -E 'ERROR|error|WARN|BUILD' || true
echo "   Build done."

# ── Step 4: Start the app ────────────────────────────────────────────────────
echo "4. Starting Spring PetClinic on port 18080..."
RAW_HPROF=$TMP_DIR/dump.hprof
APP_LOG=$TMP_DIR/app.log

java \
  -Xmx256m \
  -jar target/spring-petclinic-*.jar \
  --server.port=18080 \
  --management.endpoints.web.exposure.include=health \
  >>"$APP_LOG" 2>&1 &
APP_PID=$!
echo "   PID: $APP_PID"

# Wait up to 90s for startup
echo "   Waiting for startup..."
for i in $(seq 1 90); do
  if curl -sf http://localhost:18080/actuator/health 2>/dev/null | grep -q '"status":"UP"'; then
    echo "   App is up (${i}s)"
    break
  fi
  if ! kill -0 "$APP_PID" 2>/dev/null; then
    echo "ERROR: App process died. Log:"
    tail -30 "$APP_LOG"
    exit 1
  fi
  sleep 1
done

if ! curl -sf http://localhost:18080/actuator/health 2>/dev/null | grep -q '"status":"UP"'; then
  echo "ERROR: App did not start within 90s. Log tail:"
  tail -30 "$APP_LOG"
  kill "$APP_PID" 2>/dev/null || true
  exit 1
fi

# ── Step 5: Capture heap dump ────────────────────────────────────────────────
echo "5. Capturing heap dump (jmap)..."
jmap -dump:format=b,file="$RAW_HPROF" "$APP_PID"
echo "   Heap dump captured."

kill "$APP_PID" 2>/dev/null || true

# ── Step 6: Compress and install ─────────────────────────────────────────────
echo "6. Compressing..."
cd "$PROJ_ROOT"
mkdir -p docs/samples
gzip -9 -c "$RAW_HPROF" > "$FIXTURE_OUT"

RAW_MB=$(( $(wc -c < "$RAW_HPROF") / 1024 / 1024 ))
GZ_KB=$(( $(wc -c < "$FIXTURE_OUT") / 1024 ))
echo ""
echo "=== Done ==="
echo "Raw size:  ${RAW_MB} MB"
echo "Gzip size: ${GZ_KB} KB"
echo "Output:    $FIXTURE_OUT"
echo ""

# ── Step 7: Verify secrets are detectable ────────────────────────────────────
echo "7. Verifying secrets are present in dump..."
BIN=$(which hprof-analyzer 2>/dev/null || echo "./target/release/hprof-analyzer")

if command -v hprof-analyzer &>/dev/null; then
  echo ""
  echo "Checking for API key pattern (sk-demo-...):"
  hprof-analyzer heap query "$FIXTURE_OUT" \
    --oql 'SELECT toString(v) FROM java.lang.String v WHERE toString(v) LIKE "sk-demo-.*" LIMIT 5' \
    2>/dev/null || echo "  (query failed — check manually)"

  echo ""
  echo "Checking for JDBC URL with password:"
  hprof-analyzer heap query "$FIXTURE_OUT" \
    --oql 'SELECT toString(v) FROM java.lang.String v WHERE toString(v) LIKE "jdbc:h2:.*password=.*" LIMIT 5' \
    2>/dev/null || echo "  (query failed — check manually)"
else
  echo "  hprof-analyzer not on PATH — skipping verification queries."
  echo "  Run manually:"
  echo "    hprof-analyzer heap query $FIXTURE_OUT \\"
  echo "      --oql 'SELECT toString(v) FROM java.lang.String v WHERE toString(v) LIKE \"sk-demo-.*\" LIMIT 5'"
fi

echo ""
echo "Commit with:"
echo "  git add $FIXTURE_OUT && git commit -m 'samples: add Spring PetClinic + H2 heap dump fixture'"
