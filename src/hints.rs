// Контекстные примеры после ошибки; подсказки никогда не исполняют SQL.
use minidb::sql::{self, Command, Token};
use minidb::{Database, Kind, Schema};

pub struct Context {
    action: String,
    table: Option<String>,
    drop_column: bool,
}

impl Context {
    pub fn command(command: &Command) -> Self {
        let (action, table) = match command {
            Command::Create(schema) => ("create", Some(schema.name.as_str())),
            Command::AddColumn { table, .. } => ("alter", Some(table.as_str())),
            Command::DropColumn { table, .. } => ("drop", Some(table.as_str())),
            Command::DropForeignKey { table, .. } => ("alter", Some(table.as_str())),
            Command::Insert { table, .. } => ("insert", Some(table.as_str())),
            Command::Select { table, .. } => ("select", Some(table.as_str())),
            Command::Update { table, .. } => ("update", Some(table.as_str())),
            Command::Delete { table, .. } => ("delete", Some(table.as_str())),
            Command::Describe(table) => ("describe", Some(table.as_str())),
            Command::Vacuum(table) => ("vacuum", Some(table.as_str())),
            Command::Open(_) => ("open", None),
            Command::Backup(_) => ("backup", None),
            Command::Restore(_, _) => ("restore", None),
            _ => ("help", None),
        };
        Self {
            action: action.into(),
            table: table.map(str::to_owned),
            drop_column: false,
        }
    }

    pub fn input(input: &str) -> Self {
        let words: Vec<_> = if let Ok(tokens) = sql::tokenize(input) {
            let end = tokens
                .iter()
                .rposition(|t| !matches!(t, Token::Symbol(s) if s == ";"))
                .map_or(0, |i| i + 1);
            let start = tokens[..end]
                .iter()
                .rposition(|t| matches!(t, Token::Symbol(s) if s == ";"))
                .map_or(0, |i| i + 1);
            tokens[start..end]
                .iter()
                .filter_map(|t| match t {
                    Token::Word(s) => Some(s.to_owned()),
                    _ => None,
                })
                .collect()
        } else {
            input.split_whitespace().map(str::to_owned).collect()
        };
        let first = words
            .first()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let action = [
            "select", "insert", "update", "delete", "alter", "create", "describe", "show", "open",
            "backup", "restore", "drop", "help", "vacuum",
        ]
        .into_iter()
        .min_by_key(|candidate| distance(&first, candidate))
        .filter(|candidate| distance(&first, candidate) <= 2)
        .unwrap_or("help")
        .to_owned();
        let marker = match action.as_str() {
            "select" | "delete" => Some("from"),
            "insert" => Some("into"),
            "alter" | "create" => Some("table"),
            "drop" => Some("from"),
            _ => None,
        };
        let table = marker
            .and_then(|marker| {
                words
                    .windows(2)
                    .find(|pair| pair[0].eq_ignore_ascii_case(marker))
                    .map(|pair| pair[1].clone())
            })
            .or_else(|| {
                if matches!(
                    action.as_str(),
                    "update" | "describe" | "delete" | "insert" | "vacuum"
                ) {
                    words.get(1).cloned()
                } else {
                    None
                }
            });
        let drop_column = words.iter().any(|s| s.eq_ignore_ascii_case("column"))
            && words
                .iter()
                .any(|s| s.eq_ignore_ascii_case("drop") || s.eq_ignore_ascii_case("delete"));
        Self {
            action,
            table,
            drop_column,
        }
    }

    pub fn example(&self, db: Option<&Database>, error: &str) -> String {
        let names = db.and_then(|db| db.table_names().ok()).unwrap_or_default();
        let schema = db.and_then(|db| {
            self.table
                .as_deref()
                .and_then(|table| db.schema(table).ok())
                .or_else(|| names.first().and_then(|table| db.schema(table).ok()))
        });
        let table = schema.map(|s| s.name.as_str()).unwrap_or("notes");
        let pk = schema
            .map(|s| s.columns[s.primary_key].name.as_str())
            .unwrap_or("id");
        let mut added = "comment".to_owned();
        let mut suffix = 2;
        while schema.is_some_and(|schema| schema.column(&added).is_ok()) {
            added = format!("comment_{suffix}");
            suffix += 1;
        }
        let nullable = schema
            .and_then(|s| s.columns.iter().find(|c| c.data_type.nullable))
            .map(|c| c.name.as_str())
            .unwrap_or(&added);
        let example = if self.drop_column || self.action == "drop" {
            format!("Удалить столбец и все его значения:\n  ALTER TABLE {table} DROP COLUMN <имя_столбца>;\n  DROP COLUMN <имя_столбца> FROM {table};\nPRIMARY KEY удалить нельзя. Снять только FK, сохранив значения:\n  ALTER TABLE {table} DROP FOREIGN KEY <имя_столбца>;\nДобавить nullable-столбец:\n  ALTER TABLE {table} ADD COLUMN {added} STRING(128) NULL;\nУдалить строку с данными:\n  DELETE FROM {table} WHERE {pk} = 1;\nОчистить значение nullable-поля (после его добавления):\n  UPDATE {table} SET {nullable} = NULL WHERE {pk} = 1;")
        } else {
            match self.action.as_str() {
            "alter" => format!("Добавить столбец в конец таблицы:\n  ALTER TABLE {table} ADD COLUMN {added} STRING(128) NULL;\nБез заданного типа:\n  ALTER TABLE {table} ADD COLUMN {added};\nТип останется NULL до первого ненулевого INSERT/UPDATE. Старые строки получают NULL. Удаление: ALTER TABLE {table} DROP COLUMN <имя_столбца>;\nСнять REFERENCES: ALTER TABLE {table} DROP FOREIGN KEY <имя_столбца>;"),
            "create" => "Создать таблицу:\n  CREATE TABLE notes (id INT PRIMARY KEY, name STRING(64) NOT NULL, score FLOAT);".into(),
            "insert" => {
                let (columns, values) = schema.map(insert_fields).unwrap_or_else(|| (vec!["name".into()], vec!["'example'".into()]));
                format!("Добавить строку, указав обязательные поля ({pk} генерируется автоматически):\n  INSERT INTO {table} ({}) VALUES ({});", columns.join(", "), values.join(", "))
            }
            "select" => format!("Прочитать строки:\n  SELECT * FROM {table};\nВыбрать одну строку:\n  SELECT * FROM {table} WHERE {pk} = 1;"),
            "update" => {
                let column = schema.and_then(|s| s.columns.iter().enumerate().find(|(i, _)| *i != s.primary_key).map(|(_, c)| c));
                let (name, value) = column.map(|c| (c.name.as_str(), literal(c.data_type.kind))).unwrap_or((pk, "1".into()));
                format!("Изменить значения в строке:\n  UPDATE {table} SET {name} = {value} WHERE {pk} = 1;")
            }
            "delete" => format!("Удалить одну строку с данными:\n  DELETE FROM {table} WHERE {pk} = 1;\nУдалить ВСЕ строки, сохранив таблицу и столбцы:\n  DELETE FROM {table};\nDELETE удаляет строки; удалить столбец:\n  ALTER TABLE {table} DROP COLUMN <имя_столбца>;"),
            "describe" => format!("Посмотреть столбцы и ограничения:\n  DESCRIBE {table};"),
            "vacuum" => format!("Убрать удалённые слоты из файла данных:\n  VACUUM {table};\nСтруктура и VERSION сохраняются; GENERATION растёт при уплотнении."),
            "show" => "Показать таблицы:\n  SHOW TABLES;\nПоказать каталог базы:\n  SHOW DATABASE;".into(),
            "open" => "Открыть базу:\n  OPEN 'sample';".into(),
            "backup" => "Создать резервную копию:\n  BACKUP first;".into(),
            "restore" => "Восстановить и открыть копию:\n  RESTORE first TO 'scratch/restored';".into(),
            _ => format!("Примеры команд:\n  SELECT * FROM {table};\n  ALTER TABLE {table} ADD COLUMN {added} STRING(128) NULL;\n  DELETE FROM {table} WHERE {pk} = 1;\nВсе команды: HELP;"),
        }
        };
        let mut result = format!("Подсказка:\n{example}");
        if let Some(schema) = schema {
            if matches!(
                self.action.as_str(),
                "insert" | "select" | "update" | "delete" | "alter"
            ) || self.drop_column
            {
                let columns: Vec<_> = schema
                    .columns
                    .iter()
                    .take(12)
                    .map(|c| c.name.as_str())
                    .collect();
                result.push_str(&format!(
                    "\nСтолбцы {table}: {}{}. Полная схема: DESCRIBE {table};",
                    columns.join(", "),
                    if schema.columns.len() > columns.len() {
                        ", …"
                    } else {
                        ""
                    }
                ));
            }
        }
        if error.contains("Нет таблицы") && !names.is_empty() {
            result.push_str(&format!("\nДоступные таблицы: {}.", names.join(", ")));
        }
        if db.is_some() && names.is_empty() && self.action != "create" {
            result.push_str("\nВ базе пока нет таблиц. Создать первую:\n  CREATE TABLE notes (id INT PRIMARY KEY, name STRING(64) NOT NULL, score FLOAT);");
        }
        if self.action == "insert"
            && schema.is_some_and(|s| s.columns.iter().any(|c| c.references.is_some()))
        {
            result.push_str("\nДля внешних ключей укажите существующие id родительских таблиц.");
        }
        if error.contains("RESTRICT") || error.contains("FOREIGN KEY") {
            result.push_str(
                "\nУчитывайте внешние ключи: сначала удалите или измените ссылающиеся строки.",
            );
        }
        result
    }
}

fn insert_fields(schema: &Schema) -> (Vec<String>, Vec<String>) {
    let fields: Vec<_> = schema
        .columns
        .iter()
        .enumerate()
        .filter(|(i, c)| *i != schema.primary_key && !c.data_type.nullable)
        .map(|(_, c)| c)
        .collect();
    if fields.is_empty() {
        return (
            vec![schema.columns[schema.primary_key].name.clone()],
            vec!["NULL".into()],
        );
    }
    (
        fields.iter().map(|c| c.name.clone()).collect(),
        fields
            .iter()
            .map(|c| {
                if c.references.is_some() {
                    "1".into()
                } else {
                    literal(c.data_type.kind)
                }
            })
            .collect(),
    )
}

fn literal(kind: Kind) -> String {
    match kind {
        Kind::Null => "NULL".into(),
        Kind::Int { min, max } => 0i64.clamp(min, max).to_string(),
        Kind::Float { min, max } => format!("{:?}", 0.5f64.clamp(min, max)),
        Kind::String { max_bytes } => format!(
            "'{}'",
            "example".chars().take(max_bytes).collect::<String>()
        ),
    }
}

fn distance(left: &str, right: &str) -> usize {
    if left.len() > 32 {
        return usize::MAX;
    }
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (i, a) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in right.chars().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(old + 1)
                .min(diagonal + usize::from(a != b));
            diagonal = old;
        }
    }
    row[right.len()]
}
