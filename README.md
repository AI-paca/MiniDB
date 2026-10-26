# MiniDB

Лабораторная работа №1: собственная файловая мини-СУБД на Rust.
Готовые СУБД, ORM и JSON-хранилища не используются.

## Запуск

С Rust/Cargo 1.89+:

```sh
./run.sh          # пустая база
./run.sh sample   # база из ТЗ: 5 datasets, 5 models, 20 experiments
```

Через Docker, без Rust на хосте:

```sh
docker compose build                 # собрать один раз
docker compose run --rm minidb        # пустая база
docker compose run --rm minidb sample # база из ТЗ
```

Первый режим открывает `db/main`, второй — `db/sample`; в Docker это `/data/db`
и `/data/sample`. Повторный запуск открывает сохранённую базу. `sample` — текстовый
файл SQL-команд для наполнения, а не shell-скрипт. В консоли вводятся SQL-команды;
`HELP;` показывает справку, `EXIT;` сохраняет и завершает работу.

<details><summary>Продолжить работу с сохранённой базой</summary>

Повторный `./run.sh sample` или `docker compose run --rm minidb sample`
открывает прежние данные; команды наполнения повторно не выполняются.

Базу, созданную ранее в Docker по другому пути, можно открыть явно:

```sh
docker compose run --rm minidb /data/defense
```

Это продолжение текущего состояния. Для возврата к состоянию до ALTER нужен
сделанный заранее BACKUP; прежние поколения после переписи удаляются.

</details>

## Таблицы

| Таблица | Поля |
| --- | --- |
| datasets | id, name, task, size |
| models | id, name, version |
| experiments | id, dataset_id, model_id, accuracy, status |

PK — `id`; FK `experiments.dataset_id → datasets.id`, `model_id → models.id`.
Типы: INT, STRING(n), FLOAT. Есть NULL, авто-ID, запрет дублей и проверка FK.
Первые три experiments совпадают с примером ТЗ.

![Схема таблиц и связей из dbdiagram.io](docs/schema.png)

[Исходник DBML](docs/schema.dbml) · [Команды и примеры](docs/COMMANDS.md).

## Архитектура и хранение

CLI разбирает SQL; `Database` проверяет PK/FK и выполняет запросы.
`Table` хранит Schema, Layout и индекс `PK → offset в RAM`.
`TextFile` преобразует читаемый UTF-8 в фиксированные записи в памяти.
`Row::at(i)` читает поле по смещению из Layout; это заранее скомпилированный Rust-код.

```text
db/sample/datasets_schema   CREATE TABLE, VERSION, GENERATION, счётчик auto-ID
db/sample/datasets_1        строки LIVE/DEAD/FREE, значения через Tab
```

[Данные datasets](db/sample/datasets_1) можно читать в IDE. STRING в кавычках,
спецсимволы экранированы. CRUD сохраняет снимок через временный файл, sync и rename.
ADD/DROP COLUMN переписывают живые строки в новом поколении: данные → sync →
атомарная замена схемы → sync каталога → удаление старого файла. При открытии
убираются сироты; PK-хэш строится заново по сохранённым строкам.

<!-- architecture:start -->

![1. Архитектура: кто хранит данные и кто проверяет правила](diagrams/docs/architecture/01-architecture/01-architecture.png)

[Исходник drawio](docs/architecture/01-architecture.drawio)

<details><summary>3. Описатель записи и цена изменения колонок</summary>

![3. Описатель записи и цена изменения колонок](diagrams/docs/architecture/03-layout/03-layout.png)

[Исходник drawio](docs/architecture/03-layout.drawio)

</details>

<details><summary>4. Поиск по PK: что ускоряет индекс и что измеряется</summary>

![4. Поиск по PK: что ускоряет индекс и что измеряется](diagrams/docs/architecture/04-select/04-select.png)

[Исходник drawio](docs/architecture/04-select.drawio)

</details>

<!-- architecture:end -->

## Примеры операций и перезапуск

В базе `sample`:

```sql
CREATE TABLE notes (id INT PRIMARY KEY, text STRING(64), score FLOAT);
INSERT INTO datasets (name, task, size) VALUES ('demo', 'classification', 43);
SELECT id, name FROM datasets WHERE size >= 40;
UPDATE datasets SET size = 44 WHERE name = 'demo';
DELETE FROM datasets WHERE name = 'demo';
INSERT INTO experiments VALUES (NULL, 999, 1, 0.9, 'running');
EXIT;
```

INSERT выдаёт авто-ID; UPDATE и DELETE меняют по одной строке. Последний INSERT
отклоняется: dataset 999 отсутствует. Повторите `./run.sh sample` или Docker-запуск
с `sample`, затем `SHOW TABLES; SELECT * FROM experiments;`: notes и 20 experiments
сохранились. Сравнения: `= != > < >= <=`; есть AND/OR, выбор полей и INNER JOIN.

## Сравнение поиска

5000 строк, 1000 одинаковых запросов по PK, release. Открытие, наполнение и sync
вне таймера; данные уже в RAM. Контрольные суммы совпали: 2486500.

| Способ | Время, мс |
| --- | ---: |
| Полный просмотр `scan` | 66.159 |
| PK-индекс `get(id)` | 0.143 |
| `SELECT … WHERE id = …` | 0.134 |

Индекс выбирает один слот в среднем за O(1), scan проверяет все слоты за O(N).
Диапазоны и другие поля требуют scan; FK ищет родителя по его PK-индексу.
Это один замер; результат зависит от машины. [CSV результатов](docs/benchmark.csv).
Повторить: `cargo run --locked --offline --release --example bench -- scratch/bench 5000`.

[PDF-отчёт на две страницы](docs/report.pdf) · [Исходники проекта](https://github.com/AI-paca/MiniDB).
