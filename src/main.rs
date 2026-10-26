#[cfg(any(unix, test))]
mod completion;
mod display;
#[cfg(unix)]
mod terminal;
use display::{describe, header, print_values};
use minidb::database::DEFAULT_DATABASE_PATH;
use minidb::sql::{self, Command, Literal};
use minidb::{Database, Result, Schema, Value};
#[cfg(not(unix))]
use std::io::Write;
use std::io::{self, BufRead, IsTerminal};
use std::path::Path;

const HELP: &str = "MiniDB — команды вводятся здесь, после приглашения minidb>.
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
  INSERT INTO notes (name, score) VALUES ('Alice', 0.9);
  INSERT INTO notes (name, score) VALUES ('Bob', 0.8);
  SELECT * FROM notes;
  SELECT id, name FROM notes WHERE score >= 0.8 AND (id = 1 OR id = 2);
  UPDATE notes SET name = 'Ann' WHERE id = 1;
  DELETE FROM notes WHERE id = 2;

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
Изменения сохраняются сразу. Файлы: <таблица>.schema.sql — схема, <таблица>.vN.data — сегменты.
Запуск из Bash: bash run.sh [каталог], справка: bash run.sh -h.
";

fn main() {
    if let Err(error) = run() {
        eprintln!("Ошибка: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "gen") {
        if args.len() != 4 || args[2] != "--out" {
            return Err("Использование: minidb gen db/ --out dir/".into());
        }
        let source = minidb::codegen::generate_dir(&args[1])?;
        let out = Path::new(&args[3]);
        std::fs::create_dir_all(out)?;
        std::fs::write(out.join("tables.rs"), source)?;
        println!("Сгенерировано: {}", out.join("tables.rs").display());
        return Ok(());
    }
    if args.iter().any(|s| ["-h", "--help"].contains(&s.as_str())) {
        println!("Запуск: minidb [каталог_базы], по умолчанию sample; minidb gen db/ --out dir/\n\n{HELP}");
        return Ok(());
    }
    if args.len() > 1 {
        return Err("Использование: minidb [каталог_базы] или minidb --help".into());
    }
    let path = args
        .first()
        .map(String::as_str)
        .unwrap_or(DEFAULT_DATABASE_PATH);
    let opened = Database::open(path)?;
    let stdin = io::stdin();
    let interactive = stdin.is_terminal();
    println!(
        "MiniDB · база: {} · таблиц: {}",
        opened.directory().display(),
        opened.table_names()?.len()
    );
    println!("Введите help (или -h). Для выхода — exit или Ctrl+C.");
    let mut db = Some(opened);
    let mut input = String::new();
    let mut failed = false;
    let mut reader = stdin.lock();
    #[cfg(unix)]
    let mut editor = terminal::Editor::default();
    loop {
        let line = if interactive {
            let prompt = if input.is_empty() {
                "minidb> "
            } else {
                "   ...> "
            };
            #[cfg(unix)]
            {
                let active = db.as_ref().ok_or("Нет открытой базы")?;
                match editor.read_line(&mut reader, active.directory(), prompt, |prefix| {
                    let (start, choices) = completion::complete(active, &(input.clone() + prefix));
                    (start.saturating_sub(input.len()), choices)
                })? {
                    terminal::Input::Line(line) => line,
                    terminal::Input::Interrupt => {
                        db.as_mut().ok_or("Нет открытой базы")?.sync()?;
                        return Ok(());
                    }
                    terminal::Input::End => break,
                }
            }
            #[cfg(not(unix))]
            {
                print!("{prompt}");
                io::stdout().flush()?;
                let mut line = String::new();
                if reader.read_line(&mut line)? == 0 {
                    break;
                }
                line
            }
        } else {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            line
        };
        input.push_str(&line);
        input.push('\n');
        if unfinished(&input) {
            continue;
        }
        match sql::parse(&input) {
            Ok(commands) => {
                for command in commands {
                    match execute(&mut db, command) {
                        Ok(false) => {
                            if failed && !interactive {
                                return Err("В сценарии были ошибки".into());
                            }
                            return Ok(());
                        }
                        Ok(true) => (),
                        Err(error) => {
                            if db.is_none() {
                                return Err(error);
                            }
                            eprintln!("Ошибка: {error}");
                            failed = true;
                        }
                    }
                }
            }
            Err(error) => {
                eprintln!("Ошибка: {error}");
                failed = true;
            }
        }
        input.clear();
    }
    if !input.trim().is_empty() {
        return Err("Команда не закончена: проверьте кавычки и скобки".into());
    }
    if failed && !interactive {
        return Err("В сценарии были ошибки".into());
    }
    Ok(())
}

// Проверка незакрытых скобок и кавычек в команде
fn unfinished(text: &str) -> bool {
    let mut quote = None;
    let mut depth = 0i32;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            if c == '\\' {
                chars.next();
            } else if c == q {
                if chars.peek() == Some(&q) {
                    chars.next();
                } else {
                    quote = None;
                }
            }
        } else if c == '-' && chars.peek() == Some(&'-') {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
        }
    }
    quote.is_some() || depth > 0
}

fn restore(db: &mut Option<Database>, source: &str, target: &str) -> Result<()> {
    let active = db.as_mut().ok_or("Нет открытой базы")?;
    let source = active.backup_location(source)?;
    let current = active.directory().to_owned();
    let target = Path::new(target);
    let target = if target.exists() {
        std::fs::canonicalize(target)?
    } else {
        target.to_owned()
    };
    if target == current {
        active.sync()?;
        // Закрываем и файлы таблиц, и .lock перед заменой текущего каталога.
        drop(db.take());
    }
    match Database::restore(source, &target, &[]) {
        Ok(restored) => *db = Some(restored),
        Err(error) => {
            if db.is_none() {
                match Database::open(&current) {
                    Ok(reopened) => *db = Some(reopened),
                    Err(reopen_error) => {
                        return Err(format!("Восстановление не выполнено: {error}. Не удалось снова открыть {}: {reopen_error}", current.display()).into());
                    }
                }
            }
            return Err(error);
        }
    }
    println!(
        "Восстановлена и открыта база {}",
        db.as_ref()
            .ok_or("Нет открытой базы")?
            .directory()
            .display()
    );
    Ok(())
}

fn execute(session: &mut Option<Database>, command: Command) -> Result<bool> {
    if let Command::Restore(source, target) = &command {
        restore(session, source, target)?;
        return Ok(true);
    }
    let db = session.as_mut().ok_or("Нет открытой базы")?;
    match command {
        Command::DatabasePath => display::database_path(db),
        Command::Help => print!("{HELP}"),
        Command::Exit => return Ok(false),
        Command::Tables => {
            let names = db.table_names()?;
            if names.is_empty() {
                println!("Таблиц пока нет. Введите CREATE TABLE (пример в help).");
            }
            for name in names {
                println!("{name}");
            }
        }
        Command::Describe(name) => {
            println!("Схема: {}", db.schema_file(&name)?.display());
            for file in db.segment_files(&name)? {
                println!("Сегмент: {}", file.display());
            }
            describe(db.schema(&name)?);
        }
        Command::Open(target) => {
            if Path::new(&target).exists() && std::fs::canonicalize(&target)? == db.directory() {
                println!("Эта база уже открыта: {}", db.directory().display());
            } else {
                let opened = Database::open(&target)?;
                *db = opened;
                println!("Открыта база {}", db.directory().display());
            }
        }
        Command::Backup(target) => {
            let location = db.backup_location(&target)?;
            db.backup(&target)?;
            println!("Резервная копия: {}", location.display());
        }
        Command::Restore(_, _) => unreachable!("RESTORE обрабатывается перед заимствованием базы"),
        Command::Create(schema) => {
            let name = schema.name.clone();
            db.create(schema)?;
            println!("Создана таблица {name}");
        }
        Command::AddColumn { table, column } => {
            let name = column.name.clone();
            db.add_column(&table, column)?;
            println!("Добавлен столбец {table}.{name}");
        }
        Command::Insert {
            table,
            columns,
            values,
        } => {
            let schema = db.schema(&table)?;
            let ordered = insert_values(schema, &columns, &values)?;
            let id = db.insert(&table, &ordered)?;
            println!("Добавлена запись, id = {id}");
        }
        Command::Select {
            table,
            columns,
            join,
            condition,
        } => {
            let schema = db.schema(&table)?.clone();
            let filter = condition.expression(&schema)?;
            if let Some(join) = join {
                let right = db.schema(&join.table)?.clone();
                let (left_index, right_index) =
                    resolve_join_on(&schema, &right, &join.left, &join.right)?;
                let fields = join_fields(&schema, &right, &columns)?;
                let headers: Vec<String> = fields
                    .iter()
                    .map(|&(side, i)| {
                        let s = if side { &right } else { &schema };
                        format!("{}.{}", s.name, s.columns[i].name)
                    })
                    .collect();
                header(&headers);
                let mut buffer = vec![Value::Null; fields.len()];
                let count = db.join_on(
                    &table,
                    &schema.columns[left_index].name,
                    &join.table,
                    &right.columns[right_index].name,
                    &filter,
                    |a, b| {
                        buffer = display::print_join(
                            &headers,
                            &fields,
                            a,
                            b,
                            std::mem::take(&mut buffer),
                        )?;
                        Ok(())
                    },
                )?;
                println!("Строк: {count}");
            } else {
                let headers: Vec<String> = if columns.is_empty() {
                    schema.columns.iter().map(|c| c.name.clone()).collect()
                } else {
                    columns
                        .iter()
                        .map(|c| Ok(schema.columns[qualified(&schema, c)?].name.clone()))
                        .collect::<Result<_>>()?
                };
                let names: Vec<&str> = headers.iter().map(String::as_str).collect();
                header(&headers);
                let count = db.select(&table, &names, &filter, |values| {
                    print_values(&headers, values);
                    Ok(())
                })?;
                println!("Строк: {count}");
            }
        }
        Command::Update {
            table,
            values,
            condition,
        } => {
            let schema = db.schema(&table)?;
            let filter = condition.expression(schema)?;
            let values: Vec<(&str, Value<'_>)> = values
                .iter()
                .map(|(name, value)| {
                    Ok((
                        name.as_str(),
                        value.for_type(schema.columns[schema.column(name)?].data_type.kind)?,
                    ))
                })
                .collect::<Result<_>>()?;
            println!("Изменено записей: {}", db.update(&table, &filter, &values)?);
        }
        Command::Delete { table, condition } => {
            let filter = condition.expression(db.schema(&table)?)?;
            println!("Удалено записей: {}", db.delete(&table, &filter)?);
        }
    }
    Ok(true)
}

fn qualified(schema: &Schema, name: &str) -> Result<usize> {
    let field = if let Some((table, field)) = name.split_once('.') {
        if !table.eq_ignore_ascii_case(&schema.name) {
            return Err(format!("Ожидалось поле таблицы {}", schema.name).into());
        }
        field
    } else {
        name
    };
    schema.column(field)
}
fn resolve_join_on(left: &Schema, right: &Schema, a: &str, b: &str) -> Result<(usize, usize)> {
    let forward = qualified(left, a).and_then(|i| Ok((i, qualified(right, b)?)));
    let reverse = qualified(left, b).and_then(|i| Ok((i, qualified(right, a)?)));
    match (forward, reverse) {
        (Ok(pair), Err(_)) | (Err(_), Ok(pair)) => Ok(pair),
        (Ok(_), Ok(_)) => Err(format!("Неоднозначный ON {a} = {b}: укажите таблица.поле").into()),
        _ => Err(format!(
            "Не удалось разрешить ON {a} = {b} для таблиц {} и {}; укажите таблица.поле",
            left.name, right.name
        )
        .into()),
    }
}

fn join_fields(left: &Schema, right: &Schema, columns: &[String]) -> Result<Vec<(bool, usize)>> {
    if columns.len() > 512 {
        return Err("Проекция JOIN превышает 512 полей".into());
    }
    if columns.is_empty() {
        return Ok((0..left.columns.len())
            .map(|i| (false, i))
            .chain((0..right.columns.len()).map(|i| (true, i)))
            .collect());
    }
    columns
        .iter()
        .map(|name| {
            let a = qualified(left, name);
            let b = qualified(right, name);
            match (a, b) {
                (Ok(i), Err(_)) => Ok((false, i)),
                (Err(_), Ok(i)) => Ok((true, i)),
                (Ok(_), Ok(_)) => {
                    Err(format!("Неоднозначное поле {name}: укажите таблица.поле").into())
                }
                _ => Err(format!("Нет поля {name}").into()),
            }
        })
        .collect()
}
fn insert_values<'a>(
    schema: &Schema,
    columns: &[String],
    values: &'a [Literal],
) -> Result<Vec<Value<'a>>> {
    if columns.is_empty() {
        if values.len() != schema.columns.len() {
            return Err(
                "Число значений не совпадает со схемой. Введите describe имя_таблицы".into(),
            );
        }
        return values
            .iter()
            .zip(&schema.columns)
            .map(|(v, c)| v.for_type(c.data_type.kind))
            .collect();
    }
    if columns.len() != values.len() {
        return Err("Число колонок и значений не совпадает".into());
    }
    let mut assigned = std::collections::HashSet::new();
    let mut result = vec![Value::Null; schema.columns.len()];
    for (name, value) in columns.iter().zip(values) {
        let i = schema.column(name)?;
        if !assigned.insert(i) {
            return Err("Повторное имя столбца".into());
        }
        result[i] = value.for_type(schema.columns[i].data_type.kind)?;
    }
    Ok(result)
}
