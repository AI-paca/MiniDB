# MiniDB

Файловая база на Rust: SQL-консоль, CRUD, PK/FK, JOIN, индекс и резервные копии.
Rust/Cargo **1.89+**, внешних Rust-зависимостей нет.

## Открыть готовую базу

```sh
./run.sh
```

Без аргументов открывается **`sample/`**. При запуске печатается абсолютный путь.
В консоли:

```sql
SHOW DATABASE;
DESCRIBE datasets;
SELECT * FROM datasets;
SELECT * FROM datasets WHERE name = 'lol';
```

`SHOW DATABASE` показывает активную базу, `DESCRIBE` — её настоящие файлы и поля.
`OPEN 'другой/каталог'` переключает базу; отсутствующий каталог создаётся пустым.
Справка: `./run.sh -h` или `help` в консоли.

`sample.sh` **создаёт новый** пример с 5 datasets, 5 models и 20 experiments:

```sh
./sample.sh scratch/new-sample
./run.sh scratch/new-sample
```

При существующем каталоге скрипт отказывается его перезаписывать и показывает
команду открытия. Готовая `sample/` может содержать добавленные пользователем строки;
для работы с ней повторный запуск sample.sh не требуется. Все три таблицы нового
примера создаются сразу в v1 с обязательными полями.

В Windows консоль запускается напрямую:

```powershell
cargo build --locked --offline --release
.\target\release\minidb.exe sample
```

Для shell-скриптов нужен Git Bash или WSL. На Unix консоль поддерживает историю,
стрелки и Tab; на остальных платформах используется построчный ввод.

## Где находятся данные

```text
sample/
  datasets.schema.sql     # определение таблицы, открывается в IDE как SQL
  datasets.v1.data        # сами строки datasets в бинарном формате
  models.schema.sql
  models.v1.data
  experiments.schema.sql
  experiments.v1.data
  .lock                   # блокировка открытой базы
  .history                # история консоли
```

**`.data` — бинарные записи**, поэтому текстовый редактор показывает управляющие
символы и нулевые байты. Строки просматриваются через SELECT. Файл `.schema.sql`
содержит схему, а не содержимое таблицы. Папки с окончанием `.legacy` — архивы
старого формата, они не участвуют в INSERT/UPDATE и не обновляются.

Актуальная `sample/` не скрывается целиком в .gitignore: схемы и файлы данных
видны в проекте и входят в комплект сдачи. Игнорируются история, lock и temp.
Старые демонстрационные базы из `data/` удалены из рабочей структуры;
миграция старого формата сохранена как отдельный пример `examples/migrate.rs`.

В текущей схеме datasets обязательны `name`, `task`, `size`. Пропущенные поля
получают NULL; поэтому корректная вставка имеет вид:

```sql
INSERT INTO datasets (name, task, size) VALUES ('lol', 'fall', 43);
```

Пропущенный `id` генерируется автоматически. Ошибка обязательного поля называет
колонку, например `Поле datasets.task обязательно (NOT NULL)`.

## SQL

```sql
CREATE TABLE notes (id INT PRIMARY KEY, name STRING(64) NOT NULL, score FLOAT);
INSERT INTO notes (name, score) VALUES ('Alice', 1);
ALTER TABLE notes ADD COLUMN comment STRING(128) NULL;
SELECT id, name FROM notes WHERE score >= 0.8 AND comment IS NULL;
SELECT * FROM notes WHERE NOT (score < 0.5) OR comment IS NOT NULL;
UPDATE notes SET score = 0.95 WHERE id = 1;
DELETE FROM notes WHERE id = 1;
SELECT experiments.id, datasets.name
FROM experiments JOIN datasets ON experiments.dataset_id = datasets.id;
```

Типы: INT=i64, FLOAT=f64, STRING(n)=до n байт UTF-8. STRING без размера — 64 байта;
максимум STRING — 65535 байт. Есть NULL/NOT NULL, MIN/MAX, PRIMARY KEY и REFERENCES.
Целые SQL-литералы нормализуются для FLOAT; порядок перечисленных колонок INSERT
учитывается, остальные поля получают NULL. Имена колонок ищутся без учёта регистра.

NOT выполняется раньше AND, AND — раньше OR. Сравнения с NULL дают UNKNOWN,
который не проходит WHERE даже после NOT. Для заполненности используйте IS NOT NULL.
JOIN соединяет INT слева с PK справа; WHERE относится к левой таблице.
Неоднозначные поля требуют `таблица.поле`. Длинные значения в консоли обрезаются
символом `…` только при выводе; данные не меняются.

Синтаксис и примеры: [COMMANDS.md](COMMANDS.md).

## Версии схемы и формат

Источником определения таблицы служит `.schema.sql`: CREATE TABLE, VERSION и
COLUMNS_AT_VERSION. Схема без VERSION считается v1. Прежние `имя.schema` и
`имя.data` продолжают читаться; новый сегмент всегда называется `имя.vN.data`.
Посторонние SQL/temp-файлы не объявляют таблицы, неоднозначные дубли отклоняются.

ADD COLUMN добавляет только nullable-поля в конец и увеличивает VERSION.
В SQL требуется явное NULL. Удаление, переименование, перестановка и смена типа
запрещены `check_evolution`; DEFAULT пока не поддерживается.

Старые записи не переписываются: INSERT пишет в последний сегмент, UPDATE
из старого сегмента переносит изменённую строку и помечает прежнюю DEAD.
Логически неизменившийся UPDATE возвращает 0 и не переносит запись.
GET, SCAN, индекс и backup учитывают все сегменты. Пустые версии также сохраняются.

Запись содержит флаг, NULL-bitmap и поля. INT/FLOAT занимают 8 байт little-endian;
STRING содержит длину u16 и поле фиксированной ёмкости. Layout описывает физическую
версию, Row получает текущую схему и возвращает Null для отсутствующего поля.
Рост bitmap при 8 → 9 колонках меняет offsets: для чтения используется Layout
конкретного сегмента. Индекс связывает PK с `(версия, смещение)`.

Открытие читает флаги и PK. Поля проверяются при записи и явно через
`Row::validate` / `Table::check`; чтение проверяет bounds, длину STRING и UTF-8.

## Rust-код потребителя

```sh
cargo run --locked --offline -- gen sample --out scratch/generated
```

Генератор читает схемы и пишет tables.rs с модулем на таблицу, например
`experiments::Experiment`: публичные поля, Default, VERSION, LAYOUT, SCHEMA,
from_row, to_values, insert/update и scan. Nullable-поля и поля после v1 — Option.
Позиционного конструктора нет; используйте struct-литералы с `..Default::default()`.

API генератора: `generate(&Schema) -> Result<String>` и
`generate_dir(path) -> Result<String>`. В build.rs потребителя:

```rust,ignore
fn main() -> minidb::Result<()> {
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("tables.rs"), minidb::codegen::generate_dir("../db")?)?;
    println!("cargo:rerun-if-changed=../db");
    Ok(())
}
```

minidb нужен и в dependencies, и в build-dependencies. В коде:
`include!(concat!(env!("OUT_DIR"), "/tables.rs"));`. Пользовательские методы находятся
в отдельном impl и не генерируются. Полный пример: [examples/simulator](examples/simulator).

```sh
cargo run --offline --manifest-path examples/simulator/Cargo.toml -- scratch/simulator
```

`MINIDB_SCHEMA_DIR` задаёт build.rs абсолютный путь к другой базе. Создание из
готовой схемы vN создаёт пустые сегменты всей истории. Компилирует только потребитель,
rustc внутри БД не запускается. БД и сгенерированный код читают одни бинарные файлы.

Старый тип читает свой префикс текущей схемы; новый получает None из старой записи.
Если VERSION самого каталога ниже VERSION потребителя, чтение возвращает ошибку.
Типизированные INSERT/UPDATE требуют совпадения VERSION. Сгенерированный
`update(db, id)` меняет только поля вне PK; значение PK в структуре игнорируется. Для ручной записи через
динамический API используйте insert_versioned/check_version.

Сгенерированный scan проверяет схему один раз перед циклом. Отдельный from_row
сверяет имена/типы/ширины без полной проверки схемы на каждой строке.

## Валидаторы

`Database::open_with` принимает функции `fn(Row) -> Result<()>` по именам.
Схема с VALIDATOR требует регистрации функции. Генератор выдаёт validator_adapter,
который вызывает `from_row(row)?.validate()`; validate определяет потребитель.
VALIDATOR отсутствует в грамматике команд CLI; атрибут сохранённой схемы
доступен только через библиотеку. CLI сообщает имя необходимой функции.
OPEN/RESTORE также не обходят отсутствующие правила. При INSERT валидатор получает
подготовленную FREE-строку (`live() == false`), при UPDATE — LIVE.

## Backup и гарантии записи

```sql
BACKUP first;
RESTORE first TO 'scratch/restored';
```

Короткое имя в CLI разрешается возле текущей базы: `sample_backups/first`.
После OPEN другой базы укажите явный путь к прежней копии. Статический
`Database::restore` для совместимости разрешает короткое имя возле назначения;
для другого источника передайте явный путь или `db.backup_location(name)`.

Backup синхронизирует и копирует схемы и все сегменты. Без таблиц возвращается
ошибка; таблица со схемой и нулём строк допустима. Копия не перезаписывается.
Restore проверяет временную копию до замены назначения и возвращает прежний
каталог при ошибке установки. CLI умеет восстанавливать в текущую базу: закрывает
её до замены и открывает заново при ошибке. В библиотечном статическом restore
назначение должно быть закрыто. Суффиксы добавляются
к полному имени: `my.db.restore.tmp`, `my.db.restore.old`.

INSERT сохраняет FREE → тело → LIVE с sync_data между этапами. Незавершённый
FREE-хвост текущего сегмента дополняется до полного слота. Максимум auto-ID
восстанавливается из LIVE/DEAD; checkpoint схемы нужен при sync и перед утратой
прежнего PK при reuse или смене ключа. Обычный append не переписывает схему.

UPDATE проверяет весь пакет до записи; его предел — 16 МиБ подготовленных
изменённых строк. Большие операции нужно разбивать. DELETE использует RESTRICT,
включая self-FK. UPDATE возвращает число фактически изменённых строк; совпавший no-op даёт 0.
Буферы значений SELECT/UPDATE/JOIN переиспользуются. `join` соединяет левое INT-поле
с PK справа; `join_on` принимает и проверяет явное имя правой колонки.

**WAL и транзакционного отката нет.** I/O-ошибка UPDATE/DELETE может оставить
часть пакета применённой. Сбой переноса между сегментами может оставить два LIVE
с одинаковым PK; открытие сообщит об ошибке. Сбой ADD COLUMN может оставить
лишний сегмент; неизвестные числовые версии не игнорируются. После ошибки записи
операции таблицы запрещены до rebuild_index или повторного открытия.
Это не гарантия атомарности при отключении питания или повреждении сектора.

## Структура и проверки

- `src/database/` — операции базы, подготовка UPDATE, backup/restore.
- `src/table.rs` — сегменты, PK-индекс и файловые операции.
- `src/schema.rs`, `layout.rs`, `record.rs` — схема, раскладка и кодирование.
- `src/sql.rs`, `query.rs` — SQL и фильтры.
- `src/codegen.rs` — генератор Rust-кода из схем.
- `src/main.rs`, `display.rs`, `terminal.rs`, `completion.rs` — консоль.
- `sample/` — одна готовая рабочая база.
- `tests/` — проверки; `examples/` — benchmark, миграция и потребитель.
- `docs/` — [инварианты](docs/invariants.md), [диаграмма](docs/a.drawio),
  [DBML](docs/schema.dbml), [SVG](docs/schema.svg), [отчёт](docs/report.pdf).

```sh
cargo test --locked --offline --all-targets
cargo clippy --locked --offline --all-targets -- -D warnings
cargo fmt --all -- --check
```

Проверки выполняются на Linux. Windows и отдельный toolchain 1.89 здесь не
запускались; fsync каталога вызывается только на Unix. File::try_lock/unlock
требует Rust 1.89, HashMap::get_disjoint_mut — 1.86. Все Cargo-пакеты используют
один `target/`, заданный в .cargo/config.toml.

Benchmark и старый формат:

```sh
cargo run --locked --offline --release --example bench -- scratch/bench 5000
cargo run --locked --offline --example migrate -- /path/to/old-db
```

Мигратор читает MINIDB TEXT 2 и сохраняет оригинал в `.legacy`.
Старый бинарный WAL требует отдельного конвертера. Результаты замера
не гарантируют ту же скорость на другой машине.

Docker:

```sh
docker compose run --build --rm minidb sh /usr/local/bin/sample.sh /data/sample
docker compose run --rm minidb minidb /data/sample
```

В контейнере база передаётся явно, volume хранит данные между запусками.
