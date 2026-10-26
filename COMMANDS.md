# Команды MiniDB

## Запуск и активная база

```sh
./run.sh                 # готовая sample/
./run.sh путь/к/базе     # другая база
./sample.sh новый/путь   # новый пример; существующий каталог не перезаписывается
```

При запуске виден абсолютный путь. В консоли:

```sql
SHOW DATABASE;
SHOW TABLES;
DESCRIBE datasets;
SELECT * FROM datasets;
```

SHOW DATABASE показывает активную базу, DESCRIBE — схему, сегменты и поля.
`datasets.schema.sql` содержит определение, `datasets.vN.data` — бинарные строки.
Читать `.data` текстовым редактором не нужно: используйте SELECT.

```sql
OPEN 'scratch/other';
HELP;
EXIT;
```

OPEN переключает базу, создавая отсутствующий каталог пустым. Регистр команд
не важен; `;` разделяет команды и необязателен в конце одной строки.
Строки и пути с пробелами заключаются в одинарные/двойные кавычки.
Поддерживаются экранирования \n, \r, \t, \\, \' и \".
На Unix доступны стрелки, история, Tab, выход Ctrl+D/Ctrl+C.

## Таблицы

```sql
CREATE TABLE notes (
    id INT PRIMARY KEY,
    name STRING(64) NOT NULL,
    score FLOAT MIN 0 MAX 1,
    parent_id INT NULL REFERENCES notes(id)
);
ALTER TABLE notes ADD COLUMN comment STRING(128) NULL;
```

Типы: INT (i64), FLOAT (f64), STRING(n) (n байт UTF-8). STRING без длины — 64 байта,
разрешённый максимум — 65535 байт. Для чисел доступны MIN/MAX; обычные поля
по умолчанию допускают NULL. PK — один INT NOT NULL; REFERENCES ссылается
на PK существующей таблицы. Каскадного удаления нет.

ADD COLUMN добавляет nullable-поле только в конец, требует явное NULL и создаёт
следующий сегмент vN даже для пустой таблицы. Старые строки получают NULL для
новой колонки. DROP/переименование/смена типа, DEFAULT, CREATE INDEX и транзакционные
команды не поддерживаются. Индекс PK создаётся автоматически.

Сохранённые схемы имеют VERSION и COLUMNS_AT_VERSION. Эти строки читает
Schema::from_text, а не обычная SQL-консоль. VALIDATOR также предназначен для
библиотеки с зарегистрированной Rust-функцией через Database::open_with.
CREATE с VALIDATOR в CLI отклоняется с пояснением; OPEN/RESTORE не обходят правило.

## INSERT

```sql
INSERT INTO datasets (name, task, size) VALUES ('lol', 'fall', 43);
INSERT INTO notes VALUES (NULL, 'Alice', 1, NULL, NULL);
INSERT INTO notes (score, name) VALUES (0.8, 'Bob');
```

Порядок указанного списка колонок учитывается. Пропущенные поля получают NULL;
NULL/пропущенный PK включает auto-ID. Каждое NOT NULL поле нужно заполнить,
ошибка называет его: `Поле datasets.task обязательно (NOT NULL)`.

Целые литералы преобразуются для FLOAT и при INSERT, и при UPDATE. В Rust API
для FLOAT требуется Value::Float. Повторное имя колонки, несоответствие числа
полей, дубли PK и нарушенные FK отклоняются до записи.

## SELECT и WHERE

```sql
SELECT * FROM datasets;
SELECT id, name FROM datasets WHERE size > 100;
SELECT * FROM notes WHERE score IS NULL;
SELECT * FROM notes WHERE score IS NOT NULL;
SELECT * FROM notes WHERE NOT (score < 0.5) AND name != 'Bob';
SELECT * FROM notes WHERE name = 'Alice' OR (score >= 0.8 AND id < 10);
```

Сравнения: = != > < >= <=. Приоритет: NOT, AND, OR; скобки меняют его.
INT/FLOAT сравниваются между собой без округления большого целого в f64.
Сравнение полей, LIKE, IN, BETWEEN и `<>` не поддерживаются.

Сравнение с NULL даёт UNKNOWN; NOT сохраняет UNKNOWN. UNKNOWN не проходит WHERE.
`field != NULL` не заменяет IS NOT NULL. Имена полей ищутся без учёта регистра.
Длинные значения при выводе обрезаются символом `…`; сохранённые байты не меняются.

## JOIN

```sql
SELECT experiments.id, datasets.name
FROM experiments INNER JOIN datasets
ON experiments.dataset_id = datasets.id
WHERE experiments.accuracy >= 0.85;
```

INNER необязателен. Слева используется INT, справа — PK. Порядок сторон ON
можно поменять. Если обе комбинации полей разрешаются, укажите `таблица.поле`.
Неквалифицированная проекция также требует однозначного имени. WHERE относится
только к левой таблице. Self JOIN не поддерживается.

## UPDATE и DELETE

```sql
UPDATE notes SET score = 0.95 WHERE id = 1;
UPDATE notes SET name = 'done', comment = 'проверка' WHERE score IS NOT NULL;
DELETE FROM notes WHERE id = 2;
```

Без WHERE обрабатываются все строки. UPDATE присваивает константы и возвращает
число строк с изменившимися значениями. No-op возвращает 0 и не мигрирует старую
строку в новый сегмент. NULL в PK при UPDATE сохраняет прежний ключ.

Подготовленный пакет изменённых записей ограничен 16 МиБ. Превышение отклоняется
до записи; разбейте операцию на несколько запросов. PK/FK/пользовательские
валидаторы проверяются до сохранения пакета. RESTRICT запрещает удаление
родителя, на которого остаются ссылки; для self-FK учитывается конечный пакет.

WAL и отката при I/O-ошибках нет: UPDATE/DELETE могут примениться частично.
После ошибки записи требуется rebuild_index или повторное открытие базы.

## BACKUP и RESTORE

```sql
BACKUP first;
RESTORE first TO 'scratch/restored';
BACKUP 'scratch/explicit-backup';
RESTORE 'scratch/explicit-backup' TO 'scratch/restored-2';
```

Короткое имя в CLI разрешается рядом с текущей базой: из sample это
`sample_backups/first`. После OPEN другой базы укажите явный путь к прежней копии.
Пути относительны рабочему каталогу; run.sh запускает программу из корня проекта.

Копируются схемы и все сегменты после sync. Отсутствие таблиц — ошибка; таблица
со схемой и нулём строк допустима. Существующая копия не перезаписывается.
Restore проверяет временную копию до замены каталога. Назначение должно быть
закрыто. При восстановлении той же базы сначала переключитесь через OPEN
и затем используйте явный путь к копии. Временные суффиксы сохраняют полное имя
назначения: my.db.restore.tmp / my.db.restore.old.

В библиотеке Database::restore для совместимости разрешает короткое имя
относительно назначения; используйте явный путь или db.backup_location(name),
если копия относится к другой базе.

## Генератор Rust

Из оболочки, вне SQL-консоли:

```sh
minidb gen sample --out scratch/generated
```

Создаёт tables.rs из точных `*.schema.sql` (также прежних `*.schema`). Посторонние
SQL и temp-файлы игнорируются. Подключение через build.rs, правила в отдельном
impl и проверка VERSION описаны в [README](README.md). Пример — examples/simulator.

RESTORE в текущий каталог базы поддерживается: CLI закрывает базу перед заменой.
При отказе восстановления прежняя база открывается снова, если её файлы исправны.
