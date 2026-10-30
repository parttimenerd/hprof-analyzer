#!/bin/sh
set -eu

cd "$(dirname "$0")" || exit 1

mvn package
rm -fr dumps
mkdir -p dumps

JAR="target/test-projects.jar"

# 1. Normal: crash with a compressed heap dump.
java -Xmx300m \
    -XX:+HeapDumpOnOutOfMemoryError \
    -XX:HeapDumpGzipLevel=9 \
    -XX:HeapDumpPath=dumps/rec-normal-crash.hprof.gz \
    -cp "$JAR" RecursiveHashMaps normal 10 || true

# Capture a compressed heap dump from a running JVM.
capture_heap_dump() {
    class=$1
    name=$2
    shift 2
    args="$@"
    dump="dumps/$name.hprof.gz"

    java -cp "$JAR" $class $args &
    pid=$!

    trap 'kill "$pid" 2>/dev/null || true' EXIT HUP INT TERM

    sleep 5
    jcmd "$pid" GC.heap_dump -gz=9 "$dump"

    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    trap - EXIT HUP INT TERM
}

capture_heap_dump RecursiveHashMaps rec-complex complex 4

capture_heap_dump RecursiveHashMaps rec-normal normal 4

capture_heap_dump EmptyArrayList empty-array-list

capture_heap_dump NonClosedZipStream non-closed-zip-stream