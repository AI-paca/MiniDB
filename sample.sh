#!/bin/sh
set -eu
if [ "$#" -gt 1 ]; then
    printf 'Использование: %s [новый_каталог_базы]\n' "$0" >&2
    exit 1
fi
sample_script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
sample_root=${1:-sample}
case "$sample_root" in
    /*) ;;
    *) sample_root="$PWD/$sample_root" ;;
esac
if [ -e "$sample_root" ] || [ -L "$sample_root" ]; then
    printf 'Каталог уже существует: %s. Укажите новый каталог.\n' "$sample_root" >&2
    if [ -d "$sample_root" ]; then
        printf 'Открыть существующую базу: "%s/run.sh" "%s"\n' "$sample_script_dir" "$sample_root" >&2
    fi
    exit 1
fi
# Все таблицы и строки создаются обычными SQL-запросами через run.sh.
{
    cat <<'SQL'
-- Все таблицы создаются сразу в той же v1-схеме, что и готовая база sample/.
CREATE TABLE datasets (
    id INT PRIMARY KEY,
    name STRING(64) NOT NULL,
    task STRING(32) NOT NULL,
    size INT MIN 0 NOT NULL
);
CREATE TABLE models (
    id INT PRIMARY KEY,
    name STRING(64) NOT NULL,
    version STRING(16) NOT NULL
);
CREATE TABLE experiments (
    id INT PRIMARY KEY,
    dataset_id INT NOT NULL REFERENCES datasets(id),
    model_id INT NOT NULL REFERENCES models(id),
    accuracy FLOAT MIN 0 MAX 1 NOT NULL,
    status STRING(16) NOT NULL
);
INSERT INTO datasets VALUES (NULL, 'iris', 'classification', 150);
INSERT INTO datasets VALUES (NULL, 'mnist', 'classification', 300);
INSERT INTO datasets VALUES (NULL, 'cifar', 'classification', 450);
INSERT INTO datasets VALUES (NULL, 'imagenet', 'classification', 600);
INSERT INTO datasets VALUES (NULL, 'housing', 'regression', 750);
INSERT INTO models VALUES (NULL, 'linear', '1.0');
INSERT INTO models VALUES (NULL, 'forest', '1.0');
INSERT INTO models VALUES (NULL, 'cnn', '1.0');
INSERT INTO models VALUES (NULL, 'transformer', '1.0');
INSERT INTO models VALUES (NULL, 'baseline', '1.0');
INSERT INTO experiments VALUES (NULL, 1, 1, 0.91, 'completed');
INSERT INTO experiments VALUES (NULL, 1, 2, 0.88, 'completed');
INSERT INTO experiments VALUES (NULL, 2, 1, 0.76, 'running');
SQL
    sample_i=3
    while [ "$sample_i" -lt 20 ]; do
        sample_dataset=$((sample_i % 5 + 1))
        sample_model=$((sample_i / 5 % 5 + 1))
        sample_accuracy=$((700 + sample_i % 10 * 25))
        sample_status=completed
        if [ "$((sample_i % 3))" -eq 0 ]; then sample_status=running; fi
        printf "INSERT INTO experiments VALUES (NULL, %d, %d, 0.%03d, '%s');\n" \
            "$sample_dataset" "$sample_model" "$sample_accuracy" "$sample_status"
        sample_i=$((sample_i + 1))
    done
    printf 'exit;\n'
} | "$sample_script_dir/run.sh" --no-barriers "$sample_root"
printf 'Создана база %s: datasets (5), models (5), experiments (20).\n' "$sample_root"
