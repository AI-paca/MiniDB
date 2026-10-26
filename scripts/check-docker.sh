#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/.."

# Отдельный проект Compose и временный том; пользовательская база не используется.
project="minidb-check-$(date +%s)-$$"
logs="$(mktemp -d)"
compose() { docker compose -p "$project" "$@"; }
cleanup() {
    local result=$?
    if [ "$result" -ne 0 ]; then
        for log in "$logs"/*; do
            printf '\nDocker check log: %s\n' "$log" >&2
            tail -35 "$log" >&2
        done
    fi
    compose down --volumes --remove-orphans >/dev/null 2>&1 || true
    rm -rf -- "$logs"
}
trap cleanup EXIT
trap 'printf "Docker check failed at line %s: %s\n" "$LINENO" "$BASH_COMMAND" >&2' ERR

compose build
compose run --rm -T minidb minidb --help > "$logs/help"
grep -q 'VACUUM notes' "$logs/help"
compose run --rm -T minidb sh /usr/local/bin/sample.sh /data/sample > "$logs/seed"
printf 'SELECT * FROM datasets;\nSELECT * FROM models;\nSELECT * FROM experiments;\nEXIT;\n' |
    compose run --rm -T minidb minidb /data/sample > "$logs/counts"
test "$(grep -c '^Строк: 5$' "$logs/counts")" -eq 2
grep -qx 'Строк: 20' "$logs/counts"

compose run --rm -T minidb minidb /data/sample > "$logs/operations" <<'SQL'
HELP;
SHOW DATABASE;
SHOW TABLES;
DESCRIBE experiments;
SELECT id, dataset_id, model_id, accuracy, status FROM experiments WHERE id = 1;
SELECT id FROM experiments WHERE id != 1;
SELECT id FROM experiments WHERE id > 1;
SELECT id FROM experiments WHERE id < 3;
SELECT id FROM experiments WHERE id >= 19;
SELECT id FROM experiments WHERE id <= 2;
SELECT id FROM experiments WHERE accuracy >= 0.88 AND (status = 'completed' OR id = 3);
SELECT experiments.id, datasets.name FROM experiments INNER JOIN datasets ON experiments.dataset_id = datasets.id;
CREATE TABLE notes (id INT PRIMARY KEY, name STRING NOT NULL);
INSERT INTO notes VALUES (1, 'initial');
INSERT INTO notes VALUES (2, 'deleted later');
INSERT INTO notes VALUES (50, 'deleted maximum');
DELETE FROM notes WHERE id = 50;
ALTER TABLE notes ADD COLUMN comment STRING NULL;
UPDATE notes SET name = 'saved', comment = 'retained' WHERE id = 1;
DELETE FROM notes WHERE id = 2;
VACUUM notes;
INSERT INTO notes VALUES (NULL, 'after_vac', NULL);
CREATE TABLE schema_probe (label STRING NOT NULL, id INT PRIMARY KEY, score FLOAT NULL);
INSERT INTO schema_probe VALUES ('remove me', 1, 0.75);
DROP COLUMN label FROM schema_probe;
ALTER TABLE schema_probe ADD COLUMN parent_id INT NULL REFERENCES models(id);
UPDATE schema_probe SET parent_id = 1 WHERE id = 1;
ALTER TABLE schema_probe DROP FOREIGN KEY parent_id;
UPDATE schema_probe SET parent_id = 999 WHERE id = 1;
ALTER TABLE schema_probe ADD COLUMN linked INT NULL REFERENCES models(id);
UPDATE schema_probe SET linked = 1 WHERE id = 1;
ALTER TABLE schema_probe DROP COLUMN linked;
SELECT * FROM schema_probe WHERE id = 1;
BACKUP first;
UPDATE notes SET name = 'broken' WHERE id = 1;
RESTORE first TO '/data/sample';
SELECT * FROM notes WHERE id = 1;
OPEN '/data/other';
CREATE TABLE probe (id INT PRIMARY KEY);
INSERT INTO probe VALUES (1);
OPEN '/data/sample';
EXIT;
SQL
grep -q 'Освобождено слотов: 1' "$logs/operations"
grep -q 'Добавлена запись, id = 51' "$logs/operations"
grep -q '"saved"' "$logs/operations"
grep -q '0.91' "$logs/operations"
grep -q 'iris' "$logs/operations"
grep -qx 'Строк: 19' "$logs/operations"
test "$(grep -c '^Строк: 2$' "$logs/operations")" -eq 3
grep -qx 'Строк: 4' "$logs/operations"
grep -qx 'Строк: 20' "$logs/operations"

if printf "INSERT INTO notes VALUES (1, 'duplicate', NULL);\nEXIT;\n" |
    compose run --rm -T minidb minidb /data/sample > "$logs/duplicate" 2>&1; then
    echo 'Duplicate PK was accepted.' >&2
    exit 1
fi
grep -q 'PRIMARY KEY дублируется' "$logs/duplicate"

if printf "INSERT INTO experiments VALUES (NULL, 999, 1, 0.9, 'running');\nEXIT;\n" |
    compose run --rm -T minidb minidb /data/sample > "$logs/bad-fk" 2>&1; then
    echo 'Invalid FK was accepted.' >&2
    exit 1
fi
grep -q 'FOREIGN KEY' "$logs/bad-fk"

if printf 'ALTER TABLE schema_probe DROP COLUMN id;\nEXIT;\n' |
    compose run --rm -T minidb minidb /data/sample > "$logs/drop-pk" 2>&1; then
    echo 'Dropping PRIMARY KEY was accepted.' >&2
    exit 1
fi
grep -q 'Нельзя удалить PRIMARY KEY' "$logs/drop-pk"
compose run --rm -T minidb sh -ec '
    grep -qx "VERSION 6" /data/sample/schema_probe_schema
    grep -qx "GENERATION 6" /data/sample/schema_probe_schema
    test -f /data/sample/schema_probe_6
    grep -q "0.75" /data/sample/schema_probe_6
    grep -q "999" /data/sample/schema_probe_6
    ! grep -q "REFERENCES" /data/sample/schema_probe_schema
    ! grep -q "linked" /data/sample/schema_probe_schema
'

# Незавершённое поколение после обрыва. Следующий open должен убрать его.
compose run --rm -T minidb sh -ec 'cp /data/sample/notes_3 /data/sample/notes_99'
printf 'SELECT * FROM notes WHERE id = 1;\nSELECT * FROM notes WHERE id = 51;\nEXIT;\n' |
    compose run --rm -T minidb minidb /data/sample > "$logs/restarted"
grep -q '"saved"' "$logs/restarted"
grep -q '"after_vac"' "$logs/restarted"
test "$(grep -c '^Строк: 1$' "$logs/restarted")" -eq 2
compose run --rm -T minidb sh -ec '
    test "$(id -u)" -eq 10001
    test -f /data/sample/notes_3
    test ! -e /data/sample/notes_1
    test ! -e /data/sample/notes_2
    test ! -e /data/sample/notes_99
    grep -qx "VERSION 2" /data/sample/notes_schema
    grep -qx "GENERATION 3" /data/sample/notes_schema
'
compose run --rm -T minidb minidb gen /data/sample --out /data/generated
compose run --rm -T minidb sh -ec 'test -s /data/generated/tables.rs; grep -q "pub struct Experiment" /data/generated/tables.rs'
compose run --rm -T minidb minidb-bench /data/bench 5000 > "$logs/benchmark"
grep -qx 'rows,queries,scan_ms,index_ms,select_ms,speedup,checksum' "$logs/benchmark"
grep -q '^5000,1000,' "$logs/benchmark"
echo 'Docker: all commands, 5/5/20 rows, CRUD, six comparisons, AND/OR, JOIN, PK/FK rejection, ADD/DROP COLUMN, DROP FOREIGN KEY, VACUUM, restore, generation recovery and restart passed.'
