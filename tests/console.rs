use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "minidb-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn sample(&self) -> Output {
        Command::new("sh")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/sample.sh"))
            .arg(self.0.join("db"))
            .env("MINIDB_BIN", env!("CARGO_BIN_EXE_minidb"))
            .output()
            .unwrap()
    }
    fn run(&self, script: &str) -> Output {
        let mut process = Command::new(env!("CARGO_BIN_EXE_minidb"))
            .arg(self.0.join("db"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        process
            .stdin
            .take()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        process.wait_with_output().unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn console_crud_multiline_filters_and_restart() {
    let temp = Temp::new();
    let output = temp.run("create table notes (\nid int primary key,\nname string(200) not null, score float\n);\ninsert into notes (name, score) values ('Ann', 1);\ninsert into notes values (null, 'Bob', 0.8);\ninsert into notes values (null, 'Unicode: я; \\n \\t \\\\', null);\nselect id, name from notes where (score >= 0.8 and id != 2) or score is null;\nupdate notes set name = 'Alice', score = 0.95 where id = 1;\ndelete from notes where id = 2;\ndescribe notes;\ntables;\nexit\n");
    let stdout = success(&output);
    assert!(stdout.contains("Строк: 2"));
    assert!(stdout.contains("Изменено записей: 1"));
    assert!(stdout.contains("Удалено записей: 1"));
    assert!(!stdout.contains("Int("));
    let again = success(&temp.run("select * from notes;\nexit\n"));
    assert!(again.contains("Alice"));
    assert!(!again.contains("Bob"));
    let rows = fs::read(temp.0.join("db/notes.v1.data")).unwrap();
    let schema =
        minidb::Schema::from_text(&fs::read_to_string(temp.0.join("db/notes.schema.sql")).unwrap())
            .unwrap();
    let layout = minidb::layout::Layout::from_schema(&schema).unwrap();
    assert_eq!(rows.len(), 3 * layout.row_size);
    assert_eq!(rows[layout.row_size], minidb::record::FLAG_DEAD);
    assert!(rows.windows(5).any(|part| part == b"Alice"));
}

#[test]
fn help_flags_work_without_opening_database_and_inside_console() {
    let temp = Temp::new();
    for flag in ["-h", "--help"] {
        let output = Command::new(env!("CARGO_BIN_EXE_minidb"))
            .arg(temp.0.join("unopened"))
            .arg(flag)
            .output()
            .unwrap();
        assert!(success(&output).contains("SELECT * FROM"));
        assert!(!temp.0.join("unopened").exists());
    }
    let stdout = success(&temp.run("-h\n--help\nhelp\nquit\n"));
    assert_eq!(stdout.matches("CREATE TABLE notes").count(), 3);
}

#[test]
fn shell_sample_join_and_errors_keep_console_alive() {
    let temp = Temp::new();
    success(&temp.sample());
    let output = temp.run("show tables\nselect experiments.id, datasets.name from experiments inner join datasets on experiments.dataset_id = datasets.id where experiments.accuracy >= 0.85;\ninsert into experiments values (null, 999, 1, 0.9, 'running');\nselect *\nselect name from datasets where id = 1;\nexit\n");
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("iris"));
    assert!(stdout.contains("experiments.id"));
    assert!(stderr.contains("FOREIGN KEY"));
    assert!(stderr.contains("Ожидалось from"));
    assert!(stdout.contains("Строк: 1"));
    let stdout = success(&temp.run("select * from experiments;\nexit\n"));
    assert!(stdout.contains("Строк: 20"));
}

#[test]
fn open_backup_and_restore_commands_use_new_directory() {
    let temp = Temp::new();
    let backup = temp.0.join("snapshot with spaces");
    let restored = temp.0.join("restored");
    let other = temp.0.join("other");
    success(&temp.sample());
    let script = format!("backup '{}'\nrestore '{}' to '{}'\nselect name from datasets where id = 1;\nopen '{}'\ntables\nexit\n", backup.display(), backup.display(), restored.display(), other.display());
    let stdout = success(&temp.run(&script));
    assert!(stdout.contains("Восстановлена и открыта"));
    assert!(stdout.contains("iris"));
    assert!(stdout.contains("Таблиц пока нет"));
    assert!(backup.join("datasets.schema.sql").exists());
    assert!(restored.join("datasets.v1.data").exists());
}

#[test]
fn sample_is_sql_through_run_sh_and_refuses_to_overwrite_existing_data() {
    let temp = Temp::new();
    let stdout = success(&temp.sample());
    assert!(stdout.contains("Создана таблица datasets"));
    assert!(!stdout.contains("Добавлен столбец datasets."));
    assert!(stdout.contains("Создана таблица models"));
    assert!(!stdout.contains("Добавлен столбец models."));
    let root = temp.0.join("db");
    let before = fs::read(root.join("experiments.v1.data")).unwrap();
    let repeated = temp.sample();
    assert!(!repeated.status.success());
    assert_eq!(fs::read(root.join("experiments.v1.data")).unwrap(), before);
    let mut db = minidb::Database::open(&root).unwrap();
    assert_eq!(
        db.table_names().unwrap(),
        ["datasets", "experiments", "models"]
    );
    for (name, count) in [("datasets", 5), ("models", 5), ("experiments", 20)] {
        assert_eq!(
            db.scan(name, &minidb::Expr::All, |row| row.validate())
                .unwrap(),
            count
        );
        assert!(db.schema(name).unwrap().validator.is_none());
        assert_eq!(db.schema(name).unwrap().version, 1);
    }
    assert_eq!(
        db.schema("datasets")
            .unwrap()
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "name", "task", "size"]
    );
}

#[test]
fn alter_add_column_is_persistent_after_inserts_and_requires_explicit_null() {
    let temp = Temp::new();
    success(&temp.run("CREATE TABLE notes (id INT PRIMARY KEY);\nINSERT INTO notes VALUES (1);\nALTER TABLE notes ADD COLUMN text STRING(16) NULL;\nALTER TABLE notes ADD score FLOAT MIN 0 MAX 1 NULL;\nexit;\n"));
    let schema_path = temp.0.join("db/notes.schema.sql");
    let before = fs::read(&schema_path).unwrap();
    let data = fs::read(temp.0.join("db/notes.v1.data")).unwrap();
    let rejected = temp.run("ALTER TABLE notes ADD COLUMN text INT NULL;\nALTER TABLE notes ADD COLUMN second INT PRIMARY KEY;\nALTER TABLE notes ADD COLUMN parent INT NULL REFERENCES missing(id);\nALTER TABLE notes ADD COLUMN required INT NOT NULL;\nALTER TABLE notes ADD COLUMN implicit INT;\nexit;\n");
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("старые записи не имеют значения"));
    assert_eq!(fs::read(&schema_path).unwrap(), before);
    assert_eq!(fs::read(temp.0.join("db/notes.v1.data")).unwrap(), data);
    success(&temp.run("INSERT INTO notes VALUES (2, 'hello', 0.5);\nUPDATE notes SET text = 'old' WHERE id = 1;\nexit;\n"));
    let stdout = success(&temp.run("SELECT * FROM notes;\nALTER TABLE notes ADD COLUMN later INT NULL;\nSELECT * FROM notes;\nexit;\n"));
    assert!(stdout.contains("hello"));
    assert!(stdout.contains("old"));
    let mut db = minidb::Database::open(temp.0.join("db")).unwrap();
    assert_eq!(db.schema("notes").unwrap().version, 4);
    assert_eq!(
        db.get("notes", 1, |row| Ok(
            row.get("later")? == minidb::Value::Null
        ))
        .unwrap(),
        Some(true)
    );
}

#[test]
fn sql_insert_and_update_convert_integer_literals_for_float_columns() {
    let temp = Temp::new();
    let stdout = success(&temp.run("CREATE TABLE metrics (id INT PRIMARY KEY, size INT, accuracy FLOAT MIN 0 MAX 1, status STRING);\nINSERT INTO metrics VALUES (1, 1, 1, 'done');\nINSERT INTO metrics (accuracy, status, size) VALUES (1, 'done', 2);\nUPDATE metrics SET accuracy = 0 WHERE id = 2;\nSELECT id FROM metrics WHERE size > 1.5;\nexit;\n"));
    assert!(stdout.contains("Изменено записей: 1"));
    assert!(stdout.contains("Строк: 1"));
    let root = temp.0.join("db");
    let mut db = minidb::Database::open(&root).unwrap();
    assert_eq!(
        db.get("metrics", 1, |r| Ok(
            r.get("accuracy")? == minidb::Value::Float(1.0)
        ))
        .unwrap(),
        Some(true)
    );
    assert_eq!(
        db.get("metrics", 2, |r| Ok(
            r.get("accuracy")? == minidb::Value::Float(0.0)
        ))
        .unwrap(),
        Some(true)
    );
    drop(db);
    let stdout = success(&temp.run("UPDATE metrics SET accuracy = 1 WHERE id = 2;\nINSERT INTO metrics VALUES (3, 3, 0.3333333333333333, 'done');\nSELECT accuracy FROM metrics WHERE id = 3;\nexit;\n"));
    assert!(stdout.contains("0.333333"));
    assert!(!stdout.contains("0.3333333333333333"));
    let mut db = minidb::Database::open(&root).unwrap();
    assert_eq!(
        db.get("metrics", 2, |r| Ok(
            r.get("accuracy")? == minidb::Value::Float(1.0)
        ))
        .unwrap(),
        Some(true)
    );
    // Округляется только вывод: сохранённое значение не теряет точность.
    assert_eq!(
        db.get("metrics", 3, |r| Ok(
            r.get("accuracy")? == minidb::Value::Float(1.0 / 3.0)
        ))
        .unwrap(),
        Some(true)
    );
    drop(db);
    let stdout = success(&temp.run("CREATE TABLE precise (id INT PRIMARY KEY, score FLOAT);\nINSERT INTO precise VALUES (9007199254740993, 9007199254740992.0);\nSELECT id FROM precise WHERE id > 9007199254740992.0;\nSELECT id FROM precise WHERE score = 9007199254740993;\nexit;\n"));
    assert_eq!(stdout.matches("Строк: 1").count(), 1);
    assert_eq!(stdout.matches("Строк: 0").count(), 1);
}

#[test]
fn default_string_uses_compact_slots_and_oversized_definitions_create_no_files() {
    let temp = Temp::new();
    success(&temp.run("CREATE TABLE labels (id INT PRIMARY KEY, name STRING NOT NULL);\nINSERT INTO labels VALUES (1, 'one');\nINSERT INTO labels VALUES (2, 'two');\nINSERT INTO labels VALUES (3, 'three');\nINSERT INTO labels VALUES (4, 'four');\nINSERT INTO labels VALUES (5, 'five');\nCREATE TABLE largest (id INT PRIMARY KEY, text STRING(65535));\nexit;\n"));
    let root = temp.0.join("db");
    assert_eq!(
        fs::metadata(root.join("labels.v1.data")).unwrap().len(),
        380
    );
    let before = fs::read(root.join("labels.schema.sql")).unwrap();
    let output = temp.run("CREATE TABLE oversized (id INT PRIMARY KEY, text STRING(65536));\nCREATE TABLE huge (id INT PRIMARY KEY, text STRING(100000));\nALTER TABLE labels ADD COLUMN bad STRING(100000);\nexit;\n");
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("65535"));
    for name in ["oversized", "huge"] {
        assert!(!root.join(format!("{name}.schema.sql")).exists());
        assert!(!root.join(format!("{name}.v1.data")).exists());
    }
    assert_eq!(fs::read(root.join("labels.schema.sql")).unwrap(), before);
    assert_eq!(
        fs::metadata(root.join("labels.v1.data")).unwrap().len(),
        380
    );
}

#[test]
fn edited_column_case_is_readable_and_join_where_error_names_the_field() {
    let temp = Temp::new();
    success(&temp.sample());
    let path = temp.0.join("db/models.schema.sql");
    let schema = fs::read_to_string(&path)
        .unwrap()
        .replace("  name STRING", "  Name STRING");
    fs::write(&path, &schema).unwrap();
    let stdout = success(&temp.run("SELECT name FROM models WHERE name = 'linear';\nexit;\n"));
    assert!(stdout.contains("linear"));
    assert!(stdout.contains("Строк: 1"));
    assert_eq!(fs::read_to_string(&path).unwrap(), schema);
    let output = temp.run("SELECT experiments.id FROM experiments INNER JOIN datasets ON experiments.dataset_id = datasets.id WHERE datasets.name = 'iris';\nexit;\n");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("datasets.name"));
    assert!(stderr.contains("левой таблицы experiments"));
}

#[test]
fn restore_short_name_uses_the_current_database_when_destination_differs() {
    let temp = Temp::new();
    let destination = temp.0.join("other.db");
    let stdout = success(&temp.run(&format!("CREATE TABLE items (id INT PRIMARY KEY);\nINSERT INTO items VALUES (7);\nBACKUP first;\nRESTORE first TO '{}';\nSELECT id FROM items;\nexit;\n", destination.display())));
    assert!(stdout.contains("Восстановлена и открыта"));
    assert!(stdout.contains("Строк: 1"));
    assert!(temp.0.join("db_backups/first/items.schema.sql").exists());
    assert!(destination.join("items.v1.data").exists());
    assert!(!temp.0.join("other.db_backups").exists());
}

#[test]
fn join_resolves_both_on_orders_and_requests_qualification_when_ambiguous() {
    let temp = Temp::new();
    let stdout = success(&temp.run("CREATE TABLE parents (id INT PRIMARY KEY, name STRING);\nCREATE TABLE children (id INT PRIMARY KEY, parent_id INT);\nINSERT INTO parents VALUES (1, 'p');\nINSERT INTO children VALUES (10, 1);\nSELECT children.id FROM children JOIN parents ON parent_id = id;\nSELECT children.id FROM children JOIN parents ON id = parent_id;\nSELECT children.id FROM children JOIN parents ON parents.id = children.parent_id;\nexit;\n"));
    assert_eq!(stdout.matches("Строк: 1").count(), 3);
    let rejected = temp.run("SELECT children.id FROM children JOIN parents ON id = id;\nexit;\n");
    assert!(!rejected.status.success());
    let stderr = String::from_utf8(rejected.stderr).unwrap();
    assert!(stderr.contains("Неоднозначный ON id = id"));
    assert!(stderr.contains("таблица.поле"));
}

#[test]
fn long_output_is_truncated_and_describe_omits_storage_metadata() {
    let temp = Temp::new();
    let stdout = success(&temp.run("CREATE TABLE notes (id INT PRIMARY KEY, name STRING(200), score FLOAT);\nINSERT INTO notes (score, name, id) VALUES (0, 'очень длинное имя которое выходит за границы колонки', 7);\nSELECT id, name, score FROM notes;\nexit;\n"));
    let rows: Vec<_> = stdout
        .lines()
        .filter(|line| line.starts_with('|'))
        .collect();
    assert_eq!(rows.len(), 2);
    let pipes = |line: &str| {
        line.chars()
            .enumerate()
            .filter_map(|(i, c)| (c == '|').then_some(i))
            .collect::<Vec<_>>()
    };
    assert_eq!(pipes(rows[0]), pipes(rows[1]));
    assert!(rows[1].contains('…'));
    let mut db = minidb::Database::open(temp.0.join("db")).unwrap();
    assert_eq!(
        db.get("notes", 7, |r| Ok(r.get("score")?
            == minidb::Value::Float(0.0)
            && r.get("name")?
                == minidb::Value::String(
                    "очень длинное имя которое выходит за границы колонки"
                )))
        .unwrap(),
        Some(true)
    );
    drop(db);
    let described = success(&temp.run("DESCRIBE notes;\nexit;\n"));
    assert!(described.contains("STRING(200)"));
    assert!(described.contains("PRIMARY KEY"));
    assert!(!described.contains("COLUMNS_AT_VERSION"));
    assert!(!described.contains("VERSION "));
}

#[test]
fn cli_rejects_validator_with_a_library_instruction_without_creating_files() {
    let temp = Temp::new();
    let out =
        temp.run("CREATE TABLE guarded (id INT PRIMARY KEY) VALIDATOR business_rule;\nexit;\n");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Database::open_with"));
    assert!(!temp.0.join("db/guarded.schema.sql").exists());
    let stored = minidb::Schema::from_text(
        "CREATE TABLE guarded (id INT PRIMARY KEY) VALIDATOR business_rule;",
    )
    .unwrap();
    assert_eq!(stored.validator.as_deref(), Some("business_rule"));
}

#[test]
fn restore_to_the_active_database_releases_the_lock_and_replaces_all_segments() {
    let temp = Temp::new();
    let target = temp.0.join("db/../db");
    let stdout = success(&temp.run(&format!(
        "CREATE TABLE notes (id INT PRIMARY KEY, name STRING);\n\
         INSERT INTO notes VALUES (1, 'saved');\n\
         BACKUP first;\n\
         UPDATE notes SET name = 'changed';\n\
         ALTER TABLE notes ADD COLUMN score INT NULL;\n\
         INSERT INTO notes VALUES (2, 'after_backup', 9);\n\
         RESTORE first TO '{}';\n\
         SHOW DATABASE;\n\
         SELECT * FROM notes;\n\
         INSERT INTO notes VALUES (NULL, 'after_restore');\nexit;\n",
        target.display()
    )));
    assert!(stdout.contains("Восстановлена и открыта база"));
    assert!(stdout.contains("\"saved\""));
    assert!(!stdout.contains("\"changed\""));
    assert!(!stdout.contains("\"after_backup\""));
    assert!(!temp.0.join("db/notes.v2.data").exists());
    assert!(!temp.0.join("db.restore.tmp").exists());
    assert!(!temp.0.join("db.restore.old").exists());
    let mut db = minidb::Database::open(temp.0.join("db")).unwrap();
    assert_eq!(db.schema("notes").unwrap().version, 1);
    assert_eq!(
        db.get("notes", 2, |row| Ok(
            row.get("name")? == minidb::Value::String("after_restore")
        ))
        .unwrap(),
        Some(true)
    );
}

#[test]
fn failed_restore_reopens_the_active_database_and_keeps_the_console_usable() {
    let temp = Temp::new();
    success(&temp.run("CREATE TABLE notes (id INT PRIMARY KEY, name STRING);\nINSERT INTO notes VALUES (1, 'keep');\nexit;\n"));
    let schema = fs::read(temp.0.join("db/notes.schema.sql")).unwrap();
    let data = fs::read(temp.0.join("db/notes.v1.data")).unwrap();
    let broken = temp.0.join("broken");
    fs::create_dir(&broken).unwrap();
    fs::write(broken.join("notes.schema.sql"), &schema).unwrap();
    fs::write(broken.join("notes.v1.data"), b"invalid").unwrap();
    let output = temp.run(&format!(
        "RESTORE '{}' TO '{}';\nRESTORE '{}' TO '{}';\nSELECT * FROM notes;\nSHOW DATABASE;\nexit;\n",
        temp.0.join("missing").display(), temp.0.join("db").display(),
        broken.display(), temp.0.join("db").display()
    ));
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("\"keep\""));
    assert!(stdout.contains("Активная база:"));
    assert!(!stderr.contains("База уже открыта другим процессом"));
    assert_eq!(
        fs::read(temp.0.join("db/notes.schema.sql")).unwrap(),
        schema
    );
    assert_eq!(fs::read(temp.0.join("db/notes.v1.data")).unwrap(), data);
    assert!(!temp.0.join("db.restore.tmp").exists());
    assert!(!temp.0.join("db.restore.old").exists());
}

#[test]
fn cli_refuses_a_stored_native_validator_without_bypassing_it() {
    fn guard(_: minidb::Row<'_>) -> minidb::Result<()> {
        Ok(())
    }
    let temp = Temp::new();
    let root = temp.0.join("db");
    let mut db = minidb::Database::open_with(&root, &[("business_rule", guard)]).unwrap();
    let mut schema = minidb::Schema::new(
        "guarded",
        vec![minidb::Column::new("id", minidb::DataType::INT)],
        0,
    );
    schema.validator = Some("business_rule".into());
    db.create(schema).unwrap();
    db.insert("guarded", &[minidb::Value::Int(1)]).unwrap();
    drop(db);
    let before = fs::read(root.join("guarded.v1.data")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_minidb"))
        .arg(&root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("business_rule"));
    assert!(error.contains("Database::open_with"));
    assert!(error.contains("SQL-консоль не исполняет Rust-валидаторы"));
    assert_eq!(fs::read(root.join("guarded.v1.data")).unwrap(), before);
}

#[test]
fn default_database_and_show_database_use_the_same_canonical_directory() {
    let temp = Temp::new();
    let root = temp.0.join("sample");
    let mut db = minidb::Database::open(&root).unwrap();
    db.create(minidb::Schema::new(
        "datasets",
        vec![
            minidb::Column::new("id", minidb::DataType::INT),
            minidb::Column::new("name", minidb::DataType::STRING),
        ],
        0,
    ))
    .unwrap();
    db.insert(
        "datasets",
        &[minidb::Value::Int(6), minidb::Value::String("lol")],
    )
    .unwrap();
    drop(db);
    let mut process = Command::new(env!("CARGO_BIN_EXE_minidb"))
        .current_dir(&temp.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    process
        .stdin
        .take()
        .unwrap()
        .write_all(b"SHOW DATABASE;\nDESCRIBE datasets;\nSELECT * FROM datasets;\nexit;\n")
        .unwrap();
    let stdout = success(&process.wait_with_output().unwrap());
    let canonical = fs::canonicalize(&root).unwrap();
    assert!(stdout.contains(&format!("Активная база: {}", canonical.display())));
    assert!(stdout.contains(&canonical.join("datasets.v1.data").display().to_string()));
    assert!(stdout.contains("lol"));
    assert!(stdout.contains("Строк: 1"));
    assert!(!temp.0.join("data").exists());
}

#[test]
fn missing_required_insert_field_names_the_column_and_writes_nothing() {
    let temp = Temp::new();
    let rejected = temp.run("CREATE TABLE datasets (id INT PRIMARY KEY, name STRING NOT NULL, task STRING(32) NOT NULL, size INT NOT NULL);\nINSERT INTO datasets (name) VALUES ('lol');\nINSERT INTO datasets (name, size) VALUES ('lol', 43);\nexit;\n");
    assert!(!rejected.status.success());
    let stderr = String::from_utf8(rejected.stderr).unwrap();
    assert_eq!(
        stderr
            .matches("Поле datasets.task обязательно (NOT NULL)")
            .count(),
        2
    );
    assert_eq!(
        fs::metadata(temp.0.join("db/datasets.v1.data"))
            .unwrap()
            .len(),
        0
    );
    success(
        &temp.run("INSERT INTO datasets (name, task, size) VALUES ('lol', 'fall', 43);\nexit;\n"),
    );
    let mut db = minidb::Database::open(temp.0.join("db")).unwrap();
    assert_eq!(
        db.get("datasets", 1, |row| Ok(row.get("name")?
            == minidb::Value::String("lol")
            && row.get("task")? == minidb::Value::String("fall")))
            .unwrap(),
        Some(true)
    );
}
