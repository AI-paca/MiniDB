"""Две страницы отчёта. Генерация: python3 docs/make_report.py (нужен pycairo)."""
from pathlib import Path
import csv
import cairo

ROOT = Path(__file__).resolve().parent
WIDTH, HEIGHT = 595, 842
surface = cairo.PDFSurface(str(ROOT / 'report.pdf'), WIDTH, HEIGHT)
ctx = cairo.Context(surface)
y = 0


def text(value, x=42, size=10.5, bold=False):
    ctx.select_font_face('DejaVu Sans', cairo.FONT_SLANT_NORMAL,
                         cairo.FONT_WEIGHT_BOLD if bold else cairo.FONT_WEIGHT_NORMAL)
    ctx.set_font_size(size)
    ctx.set_source_rgb(.10, .16, .23)
    ctx.move_to(x, y)
    ctx.show_text(value)


def paragraph(value, size=10.5):
    global y
    ctx.select_font_face('DejaVu Sans', cairo.FONT_SLANT_NORMAL, cairo.FONT_WEIGHT_NORMAL)
    ctx.set_font_size(size)
    line = ''
    for word in value.split():
        candidate = f'{line} {word}'.strip()
        if ctx.text_extents(candidate).width > WIDTH - 84:
            text(line, size=size)
            y += 15
            line = word
        else:
            line = candidate
    if line:
        text(line, size=size)
        y += 15
    y += 5
    assert y < 795, 'Текст вышел за границу страницы'


def title(value):
    global y
    y += 10
    text(value, size=14, bold=True)
    y += 22


def page(number, heading):
    global y
    ctx.set_source_rgb(1, 1, 1)
    ctx.paint()
    y = 48
    text('MiniDB · файловая мини-СУБД', size=20, bold=True)
    y = 71
    text(heading, size=11)
    y = 811
    text(f'26.10.2026 · Rust · без внешних зависимостей                  {number} / 2', size=9)
    y = 101


page(1, 'Устройство, хранение и команды')
paragraph('Пример создаёт ./sample.sh: скрипт передаёт SQL в run.sh. Все три таблицы создаются сразу через CREATE TABLE; ADD COLUMN проверяется отдельно. INSERT добавляет 5/5/20 записей. Открытие: ./run.sh sample. Rust-модуля со схемами примера нет.')
title('Схема данных')
box_y = y - 4
for x, name, columns in [
    (42, 'datasets', ['id INT · PK', 'name STRING', 'task STRING', 'size INT ≥ 0']),
    (217, 'experiments', ['id INT · PK', 'dataset_id INT · FK', 'model_id INT · FK', 'accuracy FLOAT [0,1]', 'status STRING']),
    (392, 'models', ['id INT · PK', 'name STRING', 'version STRING']),
]:
    ctx.set_source_rgb(.94, .96, .99)
    ctx.rectangle(x, box_y, 161, 129)
    ctx.fill()
    y = box_y + 20
    text(name, x + 9, 11, True)
    for col in columns:
        y += 17
        text(col, x + 9, 8.8)
y = box_y + 148
paragraph('experiments.dataset_id → datasets.id; experiments.model_id → models.id. Связи N:1, RESTRICT при удалении и изменении ключа родителя. Все поля примера NOT NULL; при INSERT в datasets обязательны name, task и size. DBML для dbdiagram: docs/schema.dbml.')
title('Фиксированные записи и текстовые схемы')
paragraph('Файл имя.schema.sql содержит схему с CREATE TABLE, VERSION и COLUMNS_AT_VERSION; IDE подсвечивает SQL. Сегменты имя.vN.data содержат бинарные записи фиксированной длины в рамках своей версии. Все версии, включая первую, имеют явный номер. Прежние .schema/.data продолжают читаться. Layout хранит смещения. Запись содержит флаг, NULL-bitmap и поля: числа little-endian, STRING с двухбайтной длиной и UTF-8. Индекс PK связывает id с версией сегмента и смещением записи.')
paragraph('INSERT сохраняет FREE, затем тело с FREE, затем LIVE; между этапами — sync_data. Удаление меняет флаг на DEAD, слот можно использовать снова. Максимальный ID восстанавливается из LIVE/DEAD; checkpoint в схеме сохраняется при sync и перед утратой старого PK при reuse или смене ключа. Append не переписывает схему. Рабочего WAL нет; Пакетные UPDATE и DELETE не имеют отката при ошибке I/O. Backup копирует схемы и все сегменты после sync.')
title('Примеры команд внутри MiniDB')
for command in [
    'CREATE TABLE notes (id INT PRIMARY KEY, name STRING);',
    "INSERT INTO notes VALUES (NULL, 'Alice');",
    'SELECT name FROM notes WHERE id = 1;',
    "UPDATE notes SET name = 'Ann' WHERE id = 1;",
    'DELETE FROM notes WHERE id = 1;',
    "backup 'data/copy'",
    "restore 'data/copy' to 'data/restored'",
]:
    text(command, size=9)
    y += 16
paragraph('Также: проекция, шесть сравнений, AND/OR со скобками, IS NULL и INNER JOIN INT-поля левой таблицы с PK правой. Изменения сохраняются после каждой успешной команды.', size=10)
ctx.show_page()

page(2, 'Проверка, benchmark и ограничения')
title('Индекс: полный просмотр и точечное чтение')
paragraph('В RAM находится HashMap id → (версия сегмента, смещение записи). Scan и восстановление индекса читают через BufReader на 64 КиБ. Записи используют один рабочий буфер; поля заимствуют его память. at() декодирует поле, а ограничения проверяются отдельно при записи и через Table::check().')
with (ROOT / 'benchmark.csv').open() as file:
    bench = next(csv.DictReader(file))
paragraph(f"Release, {bench['rows']} записей, {bench['queries']} одинаковых детерминированных запросов; прогретый файловый кэш. Генерация, commit/fsync и открытие исключены из измерения поиска. Контрольные суммы результатов совпали: {bench['checksum']}.")
for name, value in [('Полный просмотр', bench['scan_ms'] + ' мс'),
                    ('Индекс id → смещение', bench['index_ms'] + ' мс'),
                    ('Отношение времени', bench['speedup'] + '×')]:
    text(name, bold=True)
    text(value, x=350)
    y += 23
paragraph('Это один локальный замер, не гарантия скорости на другой машине. Воспроизведение: cargo run --release --example bench -- scratch/bench 5000. Для другого числа записей нужен новый каталог.')
title('Проверенное поведение')
paragraph('80 тестов: CRUD и рестарт; фильтры и NULL; PK/auto-ID, FK, RESTRICT и JOIN; backup/restore и блокировка; числа, STRING и UTF-8; консоль, sample.sh, эволюция схем и перенос старого формата. SHOW DATABASE показывает абсолютный путь активной базы.')
paragraph('Тесты моделируют FREE-слот с оборванным телом, неполный хвост, повреждённые флаги и UTF-8. Проверяются сохранение максимального ID после удаления и переиспользования слота, явная проверка ограничений и отказ restore при повреждении данных.')
title('Инженерное расширение')
paragraph('SQL-подобный парсер, консоль и Docker с постоянным томом. Контейнер собирает и проверяет код, запускает консоль непривилегированным пользователем. Запуск: docker compose run --build --rm minidb. Инструкции запуска sample.sh внутри контейнера есть в README.')
title('Границы реализации')
paragraph('Индекс требует O(числа живых записей) памяти. До 256 полей; STRING по умолчанию 64 байта, максимальная ёмкость и длина — 65535 байт. Пакет UPDATE/DELETE ограничен 16 МиБ. Нет составных PK, self JOIN, vacuum и многооператорных транзакций.')
paragraph('ALTER ADD COLUMN добавляет nullable-поле в конец и новый сегмент; старые строки читают его как NULL. Генератор создаёт Rust-код из схемы; потребитель компилирует его своим build.rs. Типизированная запись проверяет VERSION. Перенос UPDATE между сегментами не транзакционен. Порядок FREE → LIVE не гарантирует атомарность при повреждении сектора. Физическое отключение питания не тестировалось.')
paragraph('Материалы: README.md, sample.sh, docs/schema.dbml, docs/schema.svg, docs/benchmark.csv.', size=10)
surface.finish()
