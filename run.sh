#!/bin/sh
set -eu
# Можно использовать уже собранную консоль (например, в контейнере).
if [ -n "${MINIDB_BIN:-}" ]; then
    exec "$MINIDB_BIN" "$@"
fi
cd "$(dirname "$0")"
cargo build --quiet --locked --offline --release --bin minidb
exec ./target/release/minidb "$@"
