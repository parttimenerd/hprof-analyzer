#!/usr/bin/env bash
# scripts/gen-spring-fixture.sh
#
# Generates docs/samples/spring-petclinic-h2-ai.hprof.gz — a Spring PetClinic
# heap dump that contains detectable secrets (AI API key, JDBC URL with password).
# This is a manual one-time step. The result is committed to git.
#
# Prerequisites:
#   - Java 17+  (java, javac on PATH)
#   - Maven 3.8+  (mvn on PATH)
#   - git clone of spring-petclinic with Spring AI support (see below)
#
# Usage:
#   bash scripts/gen-spring-fixture.sh
#
# The generated fixture is ~5-10 MB gzipped (Spring PetClinic idle heap is
# typically 80-150 MB raw).

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

FIXTURE_NAME=spring-petclinic-h2-ai.hprof.gz
FIXTURE_OUT=docs/samples/$FIXTURE_NAME
TMP_DIR=$(mktemp -d)

echo "=== Spring PetClinic + H2 + Spring AI fixture generator ==="
echo ""

# ── Step 1: Clone / build Spring PetClinic ───────────────────────────────────
PETCLINIC_DIR=$TMP_DIR/spring-petclinic

echo "1. Cloning Spring PetClinic..."
git clone --depth=1 https://github.com/spring-projects/spring-petclinic "$PETCLINIC_DIR"

# ── Step 2: Inject Spring AI + configure demo secrets ───────────────────────
# We inject the secrets directly into application.properties so they land in
# the Spring Environment (which keeps String values on the heap).
echo "2. Injecting Spring AI config with demo secrets..."

cat >> "$PETCLINIC_DIR/src/main/resources/application.properties" <<'EOF'

# Demo secrets for heap dump fixture generation
# (These are fake credentials for educational demonstration only)
spring.datasource.url=jdbc:h2:mem:petclinic;password=petclinic123
spring.datasource.username=sa
spring.datasource.password=petclinic123

# Spring AI (points at local Ollama — no real request will succeed)
spring.ai.openai.api-key=sk-demo-thisisasecret12345678901234
spring.ai.openai.base-url=http://localhost:11434/v1
EOF

# ── Step 3: Add Spring AI dependency ────────────────────────────────────────
echo "3. Adding Spring AI dependency to pom.xml..."
# Insert Spring AI BOM + openai starter into pom.xml using sed
SPRING_AI_VERSION=1.0.0
sed -i.bak "s|</dependencyManagement>|  <dependency>\n      <groupId>org.springframework.ai</groupId>\n      <artifactId>spring-ai-bom</artifactId>\n      <version>$SPRING_AI_VERSION</version>\n      <type>pom</type>\n      <scope>import</scope>\n    </dependency>\n  </dependencyManagement>|" \
  "$PETCLINIC_DIR/pom.xml" || true

sed -i.bak "s|</dependencies>|  <dependency>\n      <groupId>org.springframework.ai</groupId>\n      <artifactId>spring-ai-openai-spring-boot-starter</artifactId>\n    </dependency>\n  </dependencies>|" \
  "$PETCLINIC_DIR/pom.xml" || true

# If sed injection fails, we can still get the secrets from application.properties
# without Spring AI — that's fine for the demo.

# ── Step 4: Build ────────────────────────────────────────────────────────────
echo "4. Building Spring PetClinic (this may take a few minutes)..."
cd "$PETCLINIC_DIR"
./mvnw -q package -DskipTests 2>&1 | tail -20

# ── Step 5: Start and capture dump ───────────────────────────────────────────
echo "5. Starting application..."
RAW_HPROF=$TMP_DIR/dump.hprof

# Start in background with demo profile
java \
  -Xmx256m \
  -XX:+HeapDumpOnOutOfMemoryError \
  -jar target/spring-petclinic-*.jar \
  --server.port=18080 \
  &> $TMP_DIR/app.log &
APP_PID=$!

echo "   Waiting for app to start (PID $APP_PID)..."
for i in $(seq 1 60); do
  if curl -sf http://localhost:18080/actuator/health 2>/dev/null | grep -q '"status":"UP"'; then
    echo "   App is up after ${i}s"
    break
  fi
  sleep 1
done

# Capture heap dump
echo "6. Capturing heap dump via jmap..."
jmap -dump:format=b,file="$RAW_HPROF" "$APP_PID"

# Stop app
kill "$APP_PID" 2>/dev/null || true

# ── Step 6: Compress and install ─────────────────────────────────────────────
echo "7. Compressing heap dump..."
cd "$(git rev-parse --show-toplevel)"
mkdir -p docs/samples
gzip -9 -c "$RAW_HPROF" > "$FIXTURE_OUT"

RAW_MB=$(( $(stat -f%z "$RAW_HPROF" 2>/dev/null || stat -c%s "$RAW_HPROF") / 1024 / 1024 ))
GZ_KB=$(( $(stat -f%z "$FIXTURE_OUT" 2>/dev/null || stat -c%s "$FIXTURE_OUT") / 1024 ))
echo ""
echo "=== Done ==="
echo "Raw size:  ${RAW_MB} MB"
echo "Gzip size: ${GZ_KB} KB"
echo "Output:    $FIXTURE_OUT"
echo ""
echo "Verify with:"
echo "  hprof-analyzer heap query $FIXTURE_OUT \\"
echo "    --oql 'SELECT toString(v) FROM java.lang.String v WHERE toString(v) matches \"sk-demo.*\"'"
echo ""
echo "Expected results:"
echo "  sk-demo-thisisasecret12345678901234"
echo "  jdbc:h2:mem:petclinic;password=petclinic123"

# Cleanup
rm -rf "$TMP_DIR"
