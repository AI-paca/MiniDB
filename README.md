# MiniDB

Файловая база на Rust: SQL-консоль, CRUD, PK/FK, JOIN, индекс и резервные копии.
Rust/Cargo **1.89+**, внешних Rust-зависимостей нет.

Ветка **text** хранит данные читаемым UTF-8 без расширений. Ветка **binary**
сохраняет бинарный формат и его тесты. Переключение: `git switch binary` /
`git switch text`; `./run.sh` пересобирает консоль для выбранной ветки.

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
  datasets.schema.sql     # определение таблицы, SQL для IDE
  datasets                # сами данные, обычный UTF-8
  models.schema.sql
  models
  experiments.schema.sql
  experiments
  v2/                     # возникает после ADD COLUMN непустой таблицы
    datasets              # строки следующей версии, также UTF-8
  .lock                   # блокировка открытой базы
  .history                # история консоли
```

Откройте [sample/datasets](sample/datasets) непосредственно в IDE:

```text
# MiniDB text; VERSION 1
# state id name task size
LIVE    1  "iris" "classification" 150
LIVE    6  "lol"  "fall"           43
```

Разделитель в настоящем файле — Tab. Это рабочие файлы хранилища: INSERT/UPDATE
меняют их, RESTORE восстанавливает их. Отдельного бинарного файла с теми же
данными нет. Схема остаётся в `.schema.sql`; файлы данных не скрыты в .gitignore.
Старые базы `data/*.legacy` не участвуют в работе.

Для ручной правки сначала закройте консоль: открытая база держит рабочие строки
в памяти. После открытия IDE-изменения читаются из этих файлов. Пустая строка,
строка `"NULL"` и значение NULL различаются. Кавычки, Tab, переносы строк,
обратная косая черта и управляющие символы экранируются (`\n`, `\t`, `\u{0}`).
LIVE — действующая запись; DEAD — удалённая или перенесённая, FREE — свободный слот.

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
COLUMNS_AT_VERSION. Схема без VERSION считается v1. Прежнее имя `имя.schema`
поддерживается, но бинарные сегменты `.data` в этой ветке отклоняются с указанием
ветки binary. Посторонние SQL/temp-файлы не объявляют таблицы, дубли отклоняются.

ADD COLUMN добавляет только nullable-поля в конец и увеличивает VERSION.
В SQL требуется явное NULL. Удаление, переименование, перестановка и смена типа
запрещены `check_evolution`; DEFAULT пока не поддерживается. Пустые прошлые версии
отмечаются в схеме комментарием `-- minidb-empty-version: n`, без пустых файлов.
Исчезновение сегмента без такой отметки остаётся ошибкой.

Старые записи не переписываются: INSERT пишет в последний сегмент, UPDATE
из старого сегмента переносит изменённую строку и помечает прежнюю DEAD.
Логически неизменившийся UPDATE возвращает 0 и не переносит запись.
GET, SCAN, индекс и backup учитывают все непустые сегменты. Для пустых прошлых
версий остаётся только отметка в схеме.

На диске — заголовок VERSION и строки `LIVE<Tab>id<Tab>...`, `DEAD<Tab>...`,
`FREE`. Для v1 путь — имя таблицы, для vN — `vN/имя`. Внутренние Layout и Row
остались общим представлением в памяти для динамического API и генератора.
Ни little-endian числа, ни bitmap, ни padding на диск не записываются.
Индекс связывает PK с `(версия, смещение в памяти)`, а не смещением UTF-8 файла.
Старая запись под текущей схемой возвращает NULL для добавленного поля.

Открытие проверяет UTF-8, заголовок, число полей, типы, строковые ёмкости и PK.
Числовые MIN/MAX проверяются при записи и явно через `Row::validate` / `Table::check`.
Layout проверяется перед циклом CHECK/UPDATE, в цикле — значения строк.

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
rustc внутри БД не запускается. БД и сгенерированный код используют одни текстовые файлы через общий Database/Row.

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

Обычная запись сохраняет сегмент во временный UTF-8 файл, выполняет sync и
заменяет основной файл через rename. При ошибке до rename прежний файл остаётся
на месте. Имеющийся `.tmp` не перезаписывается и не удаляется. В Unix каталог
синхронизируется после замены. DELETE сохраняет DEAD; свободные слоты переиспользуются.
Максимум auto-ID восстанавливается из LIVE/DEAD и checkpoint схемы.

Для генерации: `./run.sh --no-barriers каталог` или
`db.set_write_barriers(false)?`. В этом режиме изменения копятся в памяти;
финальный `sync()` сохраняет их. `set_write_barriers(true)?` также выполняет sync.
sample.sh и benchmark используют этот режим при заполнении; поиск измеряется
после финального sync. При аварийном завершении seed до sync данные теряются.

UPDATE возвращает число фактически изменённых строк; no-op даёт 0. Предел пакета —
16 МиБ внутреннего представления. SELECT/UPDATE/JOIN переиспользуют буферы.
`join` соединяет левое INT-поле с PK справа; `join_on` проверяет имя правой колонки.

Это демонстрационная реализация: сегменты целиком загружаются в RAM, запись
пересохраняет текст сегмента. Benchmark сравнивает scan и индекс в памяти;
цифры не характеризуют дисковый случайный доступ и не сравниваются с binary.
**WAL и транзакционного отката нет.** При ошибке I/O пакет по нескольким сегментам
или ADD COLUMN может примениться частично. После ошибки записи нужен rebuild_index
или повторное открытие. Физическое отключение питания не тестировалось.

## Структура и проверки

- `src/database/` — операции базы, подготовка UPDATE, backup/restore.
- `src/table.rs` — сегменты и PK-индекс; `src/text_file.rs` — чтение/запись UTF-8.
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
