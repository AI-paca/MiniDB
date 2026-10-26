# Проверка инвариантов ядра — 26.10.2026

Сверка относится к текущим файлам рабочей копии. Шесть перечисленных исправлений
`table.rs` и нормализация SQL-литералов уже присутствовали к началу этой проверки.
При этой проверке исправлены лимиты STRING, смешанные числовые сравнения,
поиск столбцов по регистру и сообщение об ограничении WHERE в JOIN.

| Инвариант | Реализация | Проверка |
| --- | --- | --- |
| Неверная длина `update_bytes` возвращает ошибку до копирования и изменения файла | [table.rs](../src/table.rs), `update_bytes`: явное сравнение `bytes.len()` и `row_size` перед `Row` и `copy_from_slice` | Unit-тест `update_bytes_rejects_bad_lengths_and_flags_without_changing_data`: пустой, короткий, длинный срез; содержимое файла и запись неизменны |
| Авто-ID применяется при INSERT; NULL в PK при UPDATE сохраняет прежний PK | [table.rs](../src/table.rs), `insert`, `update`; [database/update.rs](../src/database/update.rs), `update_values` | `update_null_pk_preserves_id_and_remove_reports_presence`, `update_null_pk_preserves_parent_and_children` |
| Подготовленный INSERT имеет FREE; LIVE пишется после тела и барьера синхронизации | [table.rs](../src/table.rs), `insert` передаёт FREE в `prepare`; `insert_prepared` требует FREE, сохраняет FREE, тело, затем LIVE с `sync_data()` между этапами | Успешные INSERT проверяются через get/scan и перезапуск. `interrupted_insert_free_body_and_partial_tail_are_reusable` моделирует неполный FREE-хвост и испорченное незавершённое тело |
| Максимальный ID переживает удаление, повторное использование слота, смену PK вниз и backup/restore | [table.rs](../src/table.rs), `rebuild_index` читает PK у LIVE и DEAD; `checkpoint_highest_id` сохраняет максимум в комментарии файла схемы до перезаписи слота | `high_water_survives_slot_reuse_lower_pk_and_backup`, `legacy_dead_pk_is_preserved_before_overwriting_the_slot` |
| Отсутствующая запись отличается от удалённой | [table.rs](../src/table.rs), `remove -> Result<bool>`; [database.rs](../src/database.rs), `delete` считает только `true` | `update_null_pk_preserves_id_and_remove_reports_presence`: отсутствующий PK и повторное удаление дают false; `update_null_pk_preserves_parent_and_children`: DELETE без совпадений даёт 0 |
| Последовательный проход читает полные строки через буфер, без seek в цикле | [table.rs](../src/table.rs), `scan`, `rebuild_index`: BufReader на 64 КиБ и один row-буфер; число слотов кешируется | `buffered_scan_rebuild_and_callback_error_across_blocks`: 4000 записей, переходы между блоками, DEAD-слоты, ошибка callback и последующий get |
| Table использует кодировщик с уже проверенным Layout | [table.rs](../src/table.rs), `create`, `open`, `add_column` проверяют Layout; `prepare` вызывает `encode_validated_layout`. Публичный `record::encode` сохраняет проверку изменяемого Layout | `public_encoder_rejects_mutated_layouts_before_changing_output`, тесты INSERT/UPDATE и ADD COLUMN |
| STRING по умолчанию занимает 64 байта; n ограничен длиной u16 | [types.rs](../src/types.rs), `DEFAULT_STRING_BYTES`, `MAX_STRING_BYTES`, `DataType::check`; [layout.rs](../src/layout.rs), `validate` | `string_default_and_schema_capacity_match_the_u16_format`, `string_capacity_is_checked_in_bytes_before_mutating_output`, `default_string_uses_compact_slots_and_oversized_definitions_create_no_files`: пять строк занимают 380 байт; STRING(65536) и STRING(100000) не создают файлов |
| Целый SQL-литерал для FLOAT нормализуется до кодирования и при INSERT, и при UPDATE | [main.rs](../src/main.rs), `insert_values` в обоих режимах и ветка `Command::Update` вызывают `Literal::for_type`; Rust API хранения остаётся строгим | `sql_insert_and_update_convert_integer_literals_for_float_columns`: позиционный и именованный INSERT, UPDATE 0/1, проверка прочитанного f64 после открытия |
| ADD COLUMN добавляет nullable-поле и новый сегмент без изменения старых байтов | [schema.rs](../src/schema.rs), `check_evolution`; [table.rs](../src/table.rs), `add_column` сохраняет VERSION и историю; [layout.rs](../src/layout.rs) принимает префикс; [record.rs](../src/record.rs) возвращает Null для отсутствующего поля | `history_and_evolution_reject_destructive_changes`, `old_rows_default_null_and_bitmap_growth_uses_segment_offsets`, `alter_add_column_is_persistent_after_inserts_and_requires_explicit_null` |
| INSERT использует только последний сегмент; UPDATE переносит старую строку, сохраняя её тело | [table.rs](../src/table.rs), `append_current`, `replace_at`, `mark_dead`; индекс PK → (version, offset) | `migration_scan_update_delete_reopen_and_backup_use_all_segments`: три версии, старые free-слоты, DEAD, повторное открытие и backup/restore |
| Сгенерированный код читает общий формат и проверяет VERSION перед записью | [codegen.rs](../src/codegen.rs), `generate`, `generate_dir`, `check_reader`; [database.rs](../src/database.rs), `insert_versioned`, `check_version` | `compiled_old_and_new_consumers_share_bytes_and_reject_stale_writes`: два бинаря собираются до ALTER, v1 читает v2 после роста bitmap и отклоняет INSERT/UPDATE; v2 получает None из v1 |
| Числа INT/FLOAT можно сравнивать без округления целого в f64 | [types.rs](../src/types.rs), `compare_int_float`; [query.rs](../src/query.rs), `Expr::prepare`; [sql.rs](../src/sql.rs), `Condition::expression` сохраняет тип литерала для сравнения | `mixed_numeric_comparison_preserves_integer_precision`, `numeric_filters_compare_int_and_float_in_both_directions`, SQL-тест выше: границы i64, 2^53, отрицательные дроби и NaN |
| Исходное написание имён сохраняется, поиск столбца не зависит от регистра; неоднозначные имена запрещены | [schema.rs](../src/schema.rs), `column` использует `eq_ignore_ascii_case`, `check` выявляет дубликаты без учёта регистра | `stored_column_spelling_is_preserved_but_lookup_ignores_case`, `edited_column_case_is_readable_and_join_where_error_names_the_field` |
| WHERE в JOIN ограничен левой таблицей, ошибка сообщает введённое поле и допустимую таблицу | [sql.rs](../src/sql.rs), `field_name` | `edited_column_case_is_readable_and_join_where_error_names_the_field`: сообщение содержит `datasets.name` и `левой таблицы experiments` |
| Округление Float относится к выводу консоли и не меняет сохранённое число | [main.rs](../src/main.rs), `print_values`: до 6 знаков после точки для обычного диапазона. `Value::Display` сохраняет стандартное представление f64 | `sql_insert_and_update_convert_integer_literals_for_float_columns`: в выводе 0.333333, после открытия сохранено значение 1/3 |

| Схемы обнаруживаются по точному суффиксу; IDE видит SQL, служебные файлы не объявляют таблицы | [storage.rs](../src/storage.rs), `schema_files`: `*.schema.sql` и прежние `*.schema`; open, генератор и backup используют общий фильтр | `sql_schema_discovery_codegen_backup_and_restore_use_exact_suffixes`: README, произвольные SQL, tmp и dot-файлы игнорируются, два сегмента проходят backup/restore |
| Переименование формата не лишает доступа к старым базам и не выбирает неоднозначные копии | [storage.rs](../src/storage.rs), `schema_path`, `existing_segment`; [table.rs](../src/table.rs), `create`, `open` | `legacy_schema_and_segment_names_remain_readable_and_restorable`, `duplicate_schemas_and_segments_are_rejected_without_overwriting_data` |

| Append и DELETE не переписывают схему; checkpoint сохраняется до утраты старого PK | [table.rs](../src/table.rs), `preserve_dead_slot`, `replace_at`, `checkpoint_highest_id`, `sync` | `appends_and_deletes_keep_schema_unchanged_until_checkpoint_is_needed`, `auto_id_survives_process_exit_without_drop_or_sync` (отдельные процессы без Drop), `batch_lowering_pk_checkpoints_before_overwriting_live_maximum` |
| VERSION/history читаются обратно, дубликаты и неверные числа отклоняются | [sql.rs](../src/sql.rs), `stored_schema`; [schema.rs](../src/schema.rs), `check` | `schema_metadata_roundtrips_with_scientific_extremes_and_rejects_bad_tails`: крайние f64 в e-формате, произвольный порядок метаданных, ошибки хвоста |
| Новый потребитель отклоняет старую схему каталога; старая физическая запись под новой схемой даёт Null | [codegen.rs](../src/codegen.rs), `check_columns`, `check_schema_once`; сгенерированный scan проверяет схему перед циклом | `reader_rejects_rolled_back_schema_but_defaults_old_physical_rows`, `compiled_old_and_new_consumers_share_bytes_and_reject_stale_writes` |
| SELECT переиспользует буфер проекции без аллокации на строку | [database.rs](../src/database.rs), `project_row`: безопасный in-place collect сбрасывает ссылки между строками | [projection_allocations.rs](../tests/projection_allocations.rs): 1000 строк, 3 выделения памяти за SELECT, проверка текстовых значений и выхода по ошибке callback |

| Backup/restore без таблиц возвращает ошибку; пустая таблица со схемой допустима | [database/backup.rs](../src/database/backup.rs), `ensure_tables`, `copy_tables` | [backup_regressions.rs](../tests/backup_regressions.rs), `backup_and_restore_reject_sources_without_tables_but_allow_empty_tables` |
| Restore сохраняет полное имя назначения и ищет короткое имя в CLI возле текущей базы | `sibling` добавляет suffix к OsString; main вызывает `backup_location` перед restore | `restore_appends_suffixes_to_the_full_directory_name`, `restore_short_name_uses_the_current_database_when_destination_differs` |
| No-op UPDATE сравнивает значения и не мигрирует старую запись | [database/update.rs](../src/database/update.rs), `UpdatePlan::push`, `stage_row`; FK проверяются без Vec на строку | `unchanged_update_has_the_same_count_and_preserves_files_in_all_segments`; аллокатор: 1000 изменений — 44 аллокации, no-op — константный расход |
| NOT сохраняет UNKNOWN, IS NOT NULL работает в SQL и библиотечных фильтрах | [query.rs](../src/query.rs), `Filter::truth`; [sql.rs](../src/sql.rs), `atom` | `sql_not_uses_three_valued_logic_and_reports_plain_tokens`: NULL, NOT/AND/OR, приоритет, глубина, читаемые токены ошибок |
| ON разрешается в обоих направлениях, неоднозначность требует квалификации; вывод имеет фиксированную ширину | [main.rs](../src/main.rs), `resolve_join_on`, `cell`, `describe` | `join_resolves_both_on_orders_and_requests_qualification_when_ambiguous`, `long_output_is_truncated_and_describe_omits_storage_metadata` |

| Активная база имеет один абсолютный путь; запуск без аргументов открывает sample | [database.rs](../src/database.rs), `directory`, `DEFAULT_DATABASE_PATH`; CLI не хранит отдельную копию пути | `default_database_and_show_database_use_the_same_canonical_directory`: запуск без аргументов, SHOW DATABASE, DESCRIBE и реальная строка из файла |
| Отказ INSERT называет обязательную колонку и не пишет частичную запись | [record.rs](../src/record.rs), проверка NOT NULL до изменения буфера | `missing_required_insert_field_names_the_column_and_writes_nothing`: datasets.task, размер файла 0 после двух отказов, успешная полная вставка |

| Дополнительный инвариант | Реализация после рефакторинга | Проверка |
| --- | --- | --- |
| Подстановка auto-ID реализована один раз | [table.rs](../src/table.rs), `normalize_insert`; вызывается из Table::insert и Database::insert. В prepare подстановки нет | Тесты NULL в PK, FK после авто-ID и сохранения максимума после рестарта |
| fsync каталога не вызывается на Windows | [storage.rs](../src/storage.rs), `sync_directory` с `cfg(unix)`; backup, restore, sync и запись схемы используют helper | Backup/restore и sync выполнены на Linux; Windows-ветка не запускалась |
| Restore закрывает старый lock-дескриптор перед rename | [database/backup.rs](../src/database/backup.rs), `restore` вызывает drop(previous_lock) перед install_restore; дескрипторы временной базы уже закрыты | `backup_restore_and_exclusive_lock`, `e_backup_restore_and_failed_copy_preserve_root`, CLI backup/restore |
| Файлы копии синхронизируются через дескриптор для записи | [database/backup.rs](../src/database/backup.rs), `copy_tables`: OpenOptions::write перед sync_all | Тесты backup/restore и сравнение содержимого копии |
| Перестановка PK проверяется по конечному индексу, смещения берутся из старого | [database/update.rs](../src/database/update.rs), `apply`; [table.rs](../src/table.rs), `update_batch` | `pk_swap_is_validated_against_the_final_index`, `batch_pk_swap_across_segments_uses_original_offsets_and_rejects_outside_collisions`; текущий SQL с константами не выражает обмен одной командой |
| RESTRICT учитывает реально сохраняющиеся ссылки, включая self-FK | [database.rs](../src/database.rs), `check_references`, `check_values`; UPDATE проверяет новые ссылки по конечному набору ключей | `self_foreign_keys_restrict_external_rows_but_not_deleted_rows`, `self_foreign_key_update_checks_replacement_values_and_remaining_referrers` |
| select/validate_row/JOIN не инициализируют 256/512 значений на строку | [database.rs](../src/database.rs), [main.rs](../src/main.rs): Vec по фактическому числу полей | `projection_matches_requested_size_for_small_and_wide_tables`, тесты проекции и JOIN |
| Переполнение раскладки — явная ошибка | [layout.rs](../src/layout.rs), `from_schema -> Result<Layout>`, checked_add; [schema.rs](../src/schema.rs), `width -> Result<usize>` | `layout_construction_and_validation_report_overflow_and_short_header`; validate явно отклоняет row_size < header |
| Минимальный toolchain указан явно | Cargo.toml: rust-version = 1.89; [README](../README.md) объясняет требования API и платформы | API сверены с официальной документацией Rust; запуск выполнен на 1.98, отдельного 1.89 и Windows-target в среде нет |
| Нет неявного встроенного валидатора sample | sample.sh создаёт SQL-схемы без VALIDATOR; CLI использует Database::open, пользовательские функции — через библиотечный open_with | `sample_is_sql_through_run_sh_and_refuses_to_overwrite_existing_data`, `null_limits_and_native_validator_survive_restart` |

78 тестов прошли (включая сборку отдельных потребителей v1/v2):

```sh
cargo test --locked --offline --all-targets
cargo clippy --locked --offline --all-targets -- -D warnings
cargo fmt --all -- --check
```

Исходники тестов: [architecture.rs](../tests/architecture.rs),
[console.rs](../tests/console.rs), [database.rs](../tests/database.rs),
[record.rs](../tests/record.rs), [evolution.rs](../tests/evolution.rs), [storage_names.rs](../tests/storage_names.rs), [review_regressions.rs](../tests/review_regressions.rs), [projection_allocations.rs](../tests/projection_allocations.rs), [types.rs](../tests/types.rs),
unit-тесты [table.rs](../src/table.rs) и [database/update.rs](../src/database/update.rs).

Границы проверки: тесты прерванных вставок моделируют состояния файла,
а не физическое отключение питания. Отдельные дочерние процессы проверяют
восстановление ID после exit без Drop/sync; это также не тест отключения питания.
UPDATE на месте и пакетные UPDATE/DELETE
не имеют транзакционной защиты. Перенос между сегментами также не транзакционен:
сбой между LIVE нового слота и DEAD старого может оставить два живых PK; open
в этом случае сообщает о повреждении вместо выбора произвольной строки.
Индекс невозможно восстановить без читаемого, уникального PK живых записей.
Старые схемы с явным STRING(n) больше 65535 теперь отклоняются; изменение
дефолта не переписывает существующие файлы данных.

RESTORE в текущую базу освобождает lock и возвращает прежнюю сессию при ошибке: `restore_to_the_active_database_releases_the_lock_and_replaces_all_segments`, `failed_restore_reopens_the_active_database_and_keeps_the_console_usable`. Сгенерированный update исключает PK; компилируемые потребители проверяют несовпадающий self.id и имена Status/Address/Progress. Явный `join_on` проверяет правую колонку внутри библиотеки. Row::validate остаётся полным публичным API; Table::check и UPDATE проверяют Layout перед циклом.
