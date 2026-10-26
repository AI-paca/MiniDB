#!/bin/sh
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

# Docker entrypoint также пропускает служебные команды без подмены аргументов.
case "${1:-}" in
    minidb) shift; exec "${MINIDB_BIN:-$script_dir/target/release/minidb}" "$@" ;;
    minidb-bench|sh|bash|cat|tar) exec "$@" ;;
esac

binary=${MINIDB_BIN:-$script_dir/target/release/minidb}
if [ -z "${MINIDB_BIN:-}" ]; then
    (cd "$script_dir" && cargo build --quiet --locked --offline --release --bin minidb)
fi

database_root=${MINIDB_DATABASE_ROOT:-$script_dir/db}
if [ "$#" -eq 0 ]; then
    exec "$binary" "${MINIDB_DEFAULT_DATABASE:-$database_root/main}"
fi
if [ "$#" -eq 1 ] && [ "$1" = sample ]; then
    target=$database_root/sample
    if [ ! -e "$target" ] && [ ! -L "$target" ]; then
        source_sql=${MINIDB_SAMPLE_SQL:-$script_dir/sample}
        if [ ! -f "$source_sql" ]; then
            printf 'Нет SQL-файла примера: %s\n' "$source_sql" >&2
            exit 1
        fi
        mkdir -p "$database_root"
        staging=$(mktemp -d "$database_root/.sample.XXXXXX")
        if ! "$binary" --no-barriers "$staging" < "$source_sql" >/dev/null; then
            rm -rf -- "$staging"
            exit 1
        fi
        if ! mv -T -- "$staging" "$target"; then
            rm -rf -- "$staging"
            exit 1
        fi
    fi
    exec "$binary" "$target"
fi

# Явный путь, --help, gen и --no-barriers сохраняют CLI-семантику.
exec "$binary" "$@"
