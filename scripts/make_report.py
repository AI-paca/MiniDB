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
    assert y < 790, 'Текст вышел за границу страницы'


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
scale = min((WIDTH - 84) / image.get_width(), 210 / image.get_height())
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
paragraph('ALTER и VACUUM собирают живые строки в новом поколении, синхронизируют данные, атомарно заменяют схему, синхронизируют каталог и удаляют прежний файл. При открытии убираются осиротевшие поколения. ALTER увеличивает VERSION; VACUUM сохраняет структуру и VERSION. Максимум auto-ID сохраняется при удалении мёртвых слотов.')
title('Архитектура')
paragraph('sql.rs → Database (PK/FK, запросы, backup) → Table (один Layout, индекс id → offset, свободные слоты) → TextFile. UTF-8 декодируется в слоты в RAM; обычный CRUD сохраняет атомарный снимок того же файла. SELECT по равенству PK использует индекс; остальные условия просматривают живые строки. Drawio находятся в docs/architecture, PNG — в diagrams.', size=10)
ctx.show_page()

page(2, 'Сценарий защиты, проверки и результат индекса')
title('Воспроизведение')
paragraph('Локально: ./sample.sh scratch/defense, затем ./run.sh scratch/defense. Docker: docker compose build; docker compose run --rm minidb sh /usr/local/bin/sample.sh /data/defense; docker compose run --rm minidb minidb /data/defense. Пример содержит 5 datasets, 5 models и 20 experiments; первые три experiments — из ТЗ.', size=10)
title('Операции и ожидаемый результат')
for command in [
    "INSERT INTO datasets (name, task, size) VALUES ('demo', 'classification', 43);",
    'SELECT id, name FROM datasets WHERE size >= 100;',
    "UPDATE datasets SET size = 44 WHERE name = 'demo';",
    "DELETE FROM datasets WHERE name = 'demo';",
    "INSERT INTO experiments VALUES (NULL, 999, 1, 0.9, 'running');",
]:
    text(command, size=8.3)
    y += 16
paragraph('INSERT выдаёт авто-ID; SELECT выбирает поля с условием; UPDATE и DELETE меняют по одной строке. Последний запрос отклоняется из-за FK. После EXIT и повторного открытия остаются 20 experiments. Также поддерживаются шесть сравнений, AND/OR, NOT, IS NULL, INNER JOIN, ALTER, VACUUM и backup/restore.', size=10)
title('Сравнение поиска с индексом и без')
with (ROOT / 'docs/benchmark.csv').open() as file:
    bench = next(csv.DictReader(file))
paragraph(f"Release: {bench['rows']} строк, {bench['queries']} одинаковых запросов. Наполнение, открытие и sync вне таймера. Данные уже в RAM; это не измерение дискового I/O. Контрольные суммы всех трёх способов совпали: {bench['checksum']}.", size=10)
for label, value in [('Полный просмотр', bench['scan_ms']), ('Индекс get(id)', bench['index_ms']), ('SELECT WHERE id = …', bench['select_ms'])]:
    text(label, bold=True)
    text(value + ' мс', x=340)
    y += 21
paragraph('Повторить: cargo run --locked --offline --release --example bench -- scratch/bench 5000. Это один локальный замер; результат зависит от машины.', size=9)
title('Проверки и ограничения')
paragraph('Автотесты проверяют CRUD, перезапуск, типы, PK/FK, JOIN, фильтры, восстановление поколений и генератор структур. scripts/check-docker.sh повторяет все команды и сценарий защиты в отдельном Docker-томе. Два workflow через act проверяют экспорт drawio в README и генерацию справки из --help.', size=10)
paragraph('Docker — инженерное расширение. SQL-парсер, автотесты и генератор — бонусы поверх базовой функциональности. Генератор читает *_schema, потребитель сам компилирует Rust-структуры. WAL и многооператорных транзакций нет; UPDATE ограничен 16 МиБ. Восстановление поколений относится к полной перезаписи. Физическое отключение питания не проверялось.', size=10)
surface.finish()
