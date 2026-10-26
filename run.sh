#!/bin/sh
set -eu
cd "$(dirname "$0")"
# Можно использовать уже собранную консоль (например, в контейнере).
if [ -n "${MINIDB_BIN:-}" ]; then
    exec "$MINIDB_BIN" "$@"
fi
cargo build --quiet --locked --offline --release --bin minidb
exec ./target/release/minidb "$@"
