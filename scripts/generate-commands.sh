#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
temporary="$(mktemp "$root/docs/.COMMANDS.md.XXXXXX")"
trap 'rm -f -- "$temporary"' EXIT

{
    printf '# Команды MiniDB\n\n'
    printf '<!-- Сгенерировано scripts/generate-commands.sh; не редактировать вручную. -->\n\n'
    printf 'Вывод `minidb --help`. Обновление: `bash scripts/generate-commands.sh`.\n\n'
    printf '```text\n'
    cargo run --locked --offline --quiet --bin minidb -- --help
    printf '```\n'
} > "$temporary"

chmod 644 "$temporary"
mv -- "$temporary" "$root/docs/COMMANDS.md"
