# Команды MiniDB

<!-- Сгенерировано scripts/generate-commands.sh; не редактировать вручную. -->

Вывод `minidb --help`. Обновление: `bash scripts/generate-commands.sh`.

```text
Запуск: minidb [каталог_базы], по умолчанию sample; minidb gen db/ --out dir/

MiniDB — команды вводятся здесь, после приглашения minidb>.
Регистр команд не важен. Точка с запятой в конце одной строки необязательна.

  help | -h | --help             эта справка
  SHOW DATABASE                 абсолютный путь активной базы
  tables                        список таблиц (также SHOW TABLES)
  describe datasets             поля, типы, PK и FK (также DESC)
  open 'data/other'              открыть или создать другую базу
  backup 'data/snapshot'         резервная копия в новый каталог
  restore 'data/snapshot' to 'data/restored'  восстановить и открыть копию
  exit | quit                   сохранить и выйти (также Ctrl+C, Ctrl+D)

  CREATE TABLE notes (id INT PRIMARY KEY, name STRING NOT NULL, score FLOAT);
  ALTER TABLE notes ADD COLUMN attempts INT NULL;
  ALTER TABLE notes ADD COLUMN note;  тип NULL до первого ненулевого значения
  INSERT INTO notes (name, score) VALUES ('Alice', 0.9);
  INSERT INTO notes (name, score) VALUES ('Bob', 0.8);
  SELECT * FROM notes;
  SELECT id, name FROM notes WHERE score >= 0.8 AND (id = 1 OR id = 2);
  UPDATE notes SET name = 'Ann' WHERE id = 1;
  DELETE FROM notes WHERE id = 2;
  DELETE FROM notes;            удалить все строки, сохранив таблицу и столбцы

Добавление столбца: ALTER TABLE notes ADD COLUMN comment;
Без типа новый столбец имеет тип NULL; первый INSERT/UPDATE с ненулевым
значением фиксирует INT, FLOAT или STRING. Явный тип тоже можно задать.
Удаление столбца (DROP COLUMN) пока не поддерживается.
DELETE удаляет строки с данными. UPDATE notes SET comment = NULL WHERE id = 2
очищает значение nullable-поля, сохраняя столбец.
После ошибки сразу появляется пример команды; отдельно вызывать help не нужно.

Примеры запросов к базе sample, созданной через ./sample.sh:
  SELECT * FROM datasets;
  SELECT id, accuracy FROM experiments WHERE accuracy >= 0.85;
  SELECT experiments.id, datasets.name FROM experiments INNER JOIN datasets ON experiments.dataset_id = datasets.id;

Типы: INT, FLOAT, STRING, STRING(число_байт). Есть NULL / NOT NULL,
MIN / MAX для чисел, REFERENCES table(id), IS NULL / IS NOT NULL и NOT в WHERE.
NULL или пропущенный PK включает auto-ID. Строки — в одинарных или двойных
кавычках. Сравнения: = != > < >= <=. Приоритет: NOT, AND, OR.
JOIN: INT-поле слева → PRIMARY KEY справа, WHERE по полям левой таблицы.
RESTORE с коротким именем ищет копию в <текущая_база>_backups/<имя>.
В Unix-консоли: ↑/↓ — история, ←/→ — курсор, Tab — дополнение.
Изменения сохраняются сразу. Файлы: <таблица>.schema.sql — схема, <таблица> — UTF-8 данные; следующие версии: vN/<таблица>.
Seed без барьеров отдельных записей: bash run.sh --no-barriers каталог.
Запуск из Bash: bash run.sh [каталог], справка: bash run.sh -h.

```
