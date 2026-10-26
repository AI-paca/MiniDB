# Инварианты текстовой ветки

Ветка `text`: UTF-8 без расширений. Бинарный формат и тесты повреждённых
бинарных слотов сохранены в ветке `binary`. В текущей ветке 79 тестов.

| Инвариант | Реализация | Проверка |
| --- | --- | --- |
| Данные видны в IDE и являются рабочим хранилищем | `text_file.rs`: заголовок VERSION, состояния LIVE/DEAD/FREE и поля через Tab | `utf8_files_are_the_storage_and_ide_edits_are_read_after_reopen` |
| Схема остаётся SQL, типизированный код генерируется из неё | `schema.rs`, `codegen.rs`; потребитель сам компилирует include | `compiled_old_and_new_consumers_share_bytes_and_reject_stale_writes` |
| Все текстовые сегменты читаются через общий Row/Layout в памяти | `TextFile` декодирует UTF-8 в рабочие строки; бинарных файлов на диске нет | `migration_scan_update_delete_reopen_and_backup_use_all_segments` |
| v1 лежит в имени таблицы, vN — в каталоге vN | `storage::segment_path`, общий поиск для open/create/backup | `sql_schema_discovery_codegen_backup_and_restore_use_exact_suffixes` |
| ADD COLUMN добавляет только nullable-поле в конец | `check_evolution`, VERSION и COLUMNS_AT_VERSION | `history_and_evolution_reject_destructive_changes` |
| Пустые прошлые версии не создают мусорных файлов | Явные комментарии minidb-empty-version в схеме | `empty_alter_versions_have_metadata_without_unused_segment_files` |
| Потерянный непустой сегмент и лишняя версия не игнорируются | Проверка истории и файлов | тест выше, `unknown_versions_and_duplicate_schemas_are_not_silently_ignored` |
| Бинарная база не превращается в пустую текстовую | `existing_segment` отклоняет .data с указанием ветки binary | `binary_files_are_refused_without_creating_a_text_shadow` |
| Строковые NULL, пустая строка и значение NULL различаются | Кавычки и escapes, включая Unicode control characters | `text_roundtrips_controls_empty_strings_float_and_case`, тест IDE |
| Ошибка до rename оставляет прежний файл | UTF-8 snapshot во временный файл, sync, rename, sync каталога | `temporary_file_collision_preserves_both_data_and_the_existing_temporary_file` |
| Уже существующий временный файл не удаляется | create_new; удаляется только файл, созданный этой операцией | тот же тест |
| Некорректный UTF-8/тип/заголовок/число полей даёт ошибку с путём | `TextFile::reload`, `parse_line` | `malformed_text_is_rejected_with_a_path_and_never_rewritten` |
| Чтение не накладывает новые числовые MIN/MAX на старые данные | Чтение типа отдельно от проверки значений | `reading_does_not_apply_narrowed_constraints_but_check_does` |
| Layout проверяется перед циклами CHECK/UPDATE | В цикле вызывается внутренний validate_fields; публичный Row::validate остаётся полным | тесты record, UPDATE, backup/restore |
| Seed не выполняет fsync на каждой вставке | `set_write_barriers(false)`, CLI --no-barriers; финальный sync | `seed_without_per_record_barriers_is_persisted_before_reopen` |
| Auto-ID не повторяет удалённый или пониженный PK | LIVE/DEAD и checkpoint перед потерей максимума | `auto_id_survives_process_exit_without_drop_or_sync`, `high_water_survives_slot_reuse_lower_pk_and_backup` |
| RESTORE на место закрывает lock; отказ возвращает сессию | Option<Database> и take, повторное открытие при отказе | два теста restore в console.rs |
| Backup включает схемы и все используемые версии | Общий поиск сегментов и проверка временной копии | `seed_evolution_and_migration_use_readable_files_in_version_directories` |
| Пустая база не даёт успешный пустой backup | Проверка наличия схем | `backup_and_restore_reject_sources_without_tables_but_allow_empty_tables` |
| CLI не обходит пользовательские Rust-правила | VALIDATOR — атрибут библиотечной схемы, функции нужны в open_with | `cli_refuses_a_stored_native_validator_without_bypassing_it` |
| Сгенерированный update сохраняет PK | PK исключён из assignments | компилируемый потребитель с несовпадающим self.id |
| Имена Status/Address/Progress сохраняются | -ss/-us не сокращаются | компиляция типов в evolution.rs |
| SELECT/UPDATE/JOIN переиспользуют проекцию | In-place collect очищает заимствования | `projection_allocations.rs`, консольные JOIN |
| ON проверяется и библиотекой | join_on принимает имя правой колонки и требует PK | `fk_restrict_join_and_atomic_update` |
| No-op UPDATE даёт 0 и не мигрирует запись | Сравнение значений, не байтов разных версий | `unchanged_update_has_the_same_count_and_preserves_files_in_all_segments` |

Проверки:

```sh
cargo test --locked --offline --all-targets
cargo clippy --locked --offline --all-targets -- -D warnings
cargo fmt --check
```

Ограничения: текстовые сегменты целиком находятся в памяти; сохранение пересоздаёт
текст сегмента. Benchmark сравнивает поиск в RAM, без генерации/sync в таймере.
Между несколькими сегментами нет транзакционного отката. В seed-режиме аварийный
выход до sync теряет несохранённые изменения. Windows и физическое отключение
питания не проверялись. Строки для ручного редактирования нужно менять при закрытой БД.
