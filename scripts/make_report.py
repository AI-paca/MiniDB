"""PDF-отчёт по ТЗ на две страницы: python3 scripts/make_report.py (pycairo)."""
from pathlib import Path
import csv
import cairo

ROOT = Path(__file__).resolve().parent.parent
WIDTH, HEIGHT = 595, 842
surface = cairo.PDFSurface(str(ROOT / 'docs/report.pdf'), WIDTH, HEIGHT)
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
    leading = 14 if size <= 10 else 15
    ctx.select_font_face('DejaVu Sans', cairo.FONT_SLANT_NORMAL, cairo.FONT_WEIGHT_NORMAL)
    ctx.set_font_size(size)
    line = ''
    for word in value.split():
        candidate = f'{line} {word}'.strip()
        if ctx.text_extents(candidate).width > WIDTH - 84:
            text(line, size=size)
            y += leading
            line = word
        else:
            line = candidate
    if line:
        text(line, size=size)
        y += leading
    y += 5
    assert y < 790, f'Текст вышел за границу страницы: y={y:.1f}'


def title(value):
    global y
    y += 10
    text(value, size=13, bold=True)
    y += 22


def page(number, heading):
    global y
    ctx.set_source_rgb(1, 1, 1)
    ctx.paint()
    y = 47
    text('MiniDB · лабораторная работа №1', size=18, bold=True)
    y = 69
    text(heading, size=11)
    y = 813
    text(f'26.10.2026 · Rust · собственное файловое хранение                     {number} / 2', size=9)
    y = 101


page(1, 'Схема, устройство и формат хранения')
paragraph('Мини-СУБД реализована на Rust без внешних Rust-зависимостей и готовых СУБД. JSON не используется как основное хранилище. SQL-консоль создаёт, ищет, изменяет и удаляет строки; данные сохраняются после перезапуска.')
title('Три связанные таблицы')
image = cairo.ImageSurface.create_from_png(str(ROOT / 'docs/schema.png'))
scale = min((WIDTH - 84) / image.get_width(), 180 / image.get_height())
ctx.save()
ctx.translate((WIDTH - image.get_width() * scale) / 2, y)
ctx.scale(scale, scale)
ctx.set_source_surface(image, 0, 0)
ctx.paint()
ctx.restore()
y += image.get_height() * scale + 17
paragraph('datasets(id, name, task, size); models(id, name, version); experiments(id, dataset_id, model_id, accuracy, status). PK — id. FK experiments ссылаются на PK datasets/models. Проверяются уникальность, авто-ID, наличие родителя и RESTRICT.', size=10)
title('Собственные файлы')
paragraph('На таблицу: имя_schema — CREATE TABLE, VERSION, GENERATION и счётчик auto-ID; имя_N — единственные текущие данные, N берётся из GENERATION. UTF-8: заголовок, состояния LIVE/DEAD/FREE, значения через Tab. Строки заключены в кавычки, специальные символы экранированы. INT=i64, FLOAT=f64, STRING(n)=UTF-8 до n байт.')
paragraph('ALTER (ADD/DROP COLUMN, снятие FK) и VACUUM собирают живые строки в новом поколении: sync данных → атомарная замена схемы → sync каталога → удаление прежнего файла. Открытие убирает осиротевшие поколения. ALTER увеличивает VERSION и GENERATION; VACUUM — только GENERATION, если есть мёртвые слоты. Максимум auto-ID сохраняется.')
title('Архитектура')
paragraph('sql.rs → Database (PK/FK, запросы, backup) → Table → TextFile. Описатель разделён на Schema (поля и ограничения) и Layout (NULL-bitmap, ширины, смещения в RAM). Row заимствует оба описателя и байты слота. UTF-8 декодируется в RAM; обычный CRUD сохраняет атомарный снимок текущего файла. Индекс PK → offset адресует слот в RAM, не байт текстового файла.', size=10)
paragraph('ADD/DROP перекодируют живые строки одной таблицы: оставшиеся поля сопоставляются по имени, новые получают NULL. Ядро не пересобирается; одновременно в RAM находятся старый и новый буферы. До переключения на диске тоже лежат два поколения.', size=10)
ctx.show_page()

page(2, 'Запуск, примеры операций и результат индекса')
paragraph('Локально нужен Rust/Cargo 1.89+. Для Docker Rust на хосте не нужен: один раз выполните docker compose build. Два режима запуска:', size=10)
for command, mode in [
    ('./run.sh', 'пустая база'),
    ('./run.sh sample', 'база из ТЗ'),
    ('docker compose run --rm minidb', 'пустая база'),
    ('docker compose run --rm minidb sample', 'база из ТЗ'),
]:
    text(command, size=9)
    text(mode, x=390, size=9)
    y += 16
paragraph('Локальные пути: db/main и db/sample; в Docker: /data/db и /data/sample. sample — текстовый SQL-файл наполнения. Повторный запуск открывает сохранённую базу. Пример: 5 datasets, 5 models и 20 experiments; первые три experiments — из ТЗ. HELP; показывает справку.', size=10)
title('Операции и ожидаемый результат')
for command in [
    'CREATE TABLE notes (id INT PRIMARY KEY, text STRING(64), score FLOAT);',
    "INSERT INTO datasets (name, task, size) VALUES ('demo', 'classification', 43);",
    'SELECT id, name FROM datasets WHERE size >= 40;',
    "UPDATE datasets SET size = 44 WHERE name = 'demo';",
    "DELETE FROM datasets WHERE name = 'demo';",
    "INSERT INTO experiments VALUES (NULL, 999, 1, 0.9, 'running');",
    'EXIT;',
]:
    text(command, size=8.3)
    y += 16
paragraph('INSERT выдаёт авто-ID; UPDATE и DELETE меняют по одной строке. INSERT с dataset 999 отклоняется из-за FK. Повторите запуск с sample, затем SHOW TABLES; SELECT * FROM experiments; — notes и 20 experiments сохранились. Поддерживаются шесть сравнений (=, !=, >, <, >=, <=), AND/OR, выбор полей и INNER JOIN.', size=10)
title('Сравнение поиска с индексом и без')
with (ROOT / 'docs/benchmark.csv').open() as file:
    bench = next(csv.DictReader(file))
paragraph(f"Release: {bench['rows']} строк, {bench['queries']} одинаковых запросов. Наполнение, открытие и sync вне таймера. Данные уже в RAM; это не измерение дискового I/O. Контрольные суммы всех трёх способов совпали: {bench['checksum']}.", size=10)
paragraph('Просмотр — O(числа слотов); HashMap по равенству INT PK — в среднем O(1), затем проверяется весь WHERE. Диапазоны и другие поля требуют просмотра. Индекс строится при открытии и обновляется при CRUD; после переписи перестраивается.', size=10)
for label, value in [('Полный просмотр', bench['scan_ms']), ('Индекс get(id)', bench['index_ms']), ('SELECT WHERE id = …', bench['select_ms'])]:
    text(label, bold=True)
    text(value + ' мс', x=340)
    y += 21
paragraph('Повторить: cargo run --locked --offline --release --example bench -- scratch/bench 5000. Это один локальный замер; результат зависит от машины.', size=9)
paragraph('Исходники: https://github.com/AI-paca/MiniDB. Данные примера: db/sample; схема dbdiagram.io: docs/schema.dbml; результаты замера: docs/benchmark.csv.', size=9)
surface.finish()
