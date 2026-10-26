// Дополнение команд, имён таблиц и полей по Tab
use minidb::sql::{tokenize, Token};
use minidb::Database;

pub fn complete(db: &Database, text: &str) -> (usize, Vec<String>) {
    let start = text
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_alphanumeric() && !"_.-".contains(*c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let prefix = &text[start..];
    let Ok(tokens) = tokenize(&text[..start]) else {
        return (start, Vec::new());
    };
    let tokens: Vec<String> = tokens
        .iter()
        .filter_map(|token| match token {
            Token::Word(s) | Token::Symbol(s) => Some(s.to_lowercase()),
            _ => None,
        })
        .collect();
    let last = tokens.last().map(String::as_str).unwrap_or("");
    let names = db.table_names().unwrap_or_default();
    let mut options = Vec::new();
    if let Some((table, _)) = prefix.split_once('.') {
        if let Ok(schema) = db.schema(table) {
            options.extend(schema.columns.iter().map(|c| format!("{table}.{}", c.name)));
        }
    } else if tokens.is_empty() || last == ";" {
        options.extend(
            [
                "help", "tables", "describe", "open", "backup", "restore", "exit", "SELECT",
                "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "SHOW", "VACUUM",
            ]
            .map(str::to_owned),
        );
    } else if [
        "from",
        "into",
        "update",
        "describe",
        "desc",
        "join",
        "references",
        "vacuum",
    ]
    .contains(&last)
    {
        options.extend(names.iter().map(|s| s.to_string()));
    } else if last == "create" || last == "alter" {
        options.push("TABLE".into());
    } else if last == "insert" {
        options.push("INTO".into());
    } else if last == "delete" {
        options.push("FROM".into());
    } else if last == "show" {
        options.extend(["TABLES", "DATABASE"].map(str::to_owned));
    } else if last == "inner" {
        options.push("JOIN".into());
    } else {
        let table = tokens
            .windows(2)
            .rev()
            .find(|w| ["from", "into", "update"].contains(&w[0].as_str()))
            .map(|w| w[1].as_str());
        for name in &names {
            if table.is_none() || table == Some(*name) || last == "on" {
                if let Ok(schema) = db.schema(name) {
                    options.extend(schema.columns.iter().map(|c| c.name.clone()));
                }
            }
        }
        options.extend(
            [
                "FROM",
                "WHERE",
                "AND",
                "OR",
                "IS",
                "NULL",
                "SET",
                "VALUES",
                "INNER",
                "JOIN",
                "ON",
                "INT",
                "FLOAT",
                "STRING",
                "NOT",
                "PRIMARY",
                "KEY",
                "REFERENCES",
                "ADD",
                "COLUMN",
                "DROP",
                "FOREIGN",
                "MIN",
                "MAX",
            ]
            .map(str::to_owned),
        );
    }
    options.retain(|s| s.to_lowercase().starts_with(&prefix.to_lowercase()));
    options.sort();
    options.dedup();
    (start, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keywords_tables_and_columns() -> minidb::Result<()> {
        let path = std::env::temp_dir().join(format!("minidb-completion-{}", std::process::id()));
        let mut db = Database::open(&path)?;
        db.create(minidb::Schema::new(
            "datasets",
            vec![
                minidb::Column::new("id", minidb::DataType::INT),
                minidb::Column::new("size", minidb::DataType::INT),
            ],
            0,
        ))?;
        db.create(minidb::Schema::new(
            "experiments",
            vec![
                minidb::Column::new("id", minidb::DataType::INT),
                minidb::Column::new("accuracy", minidb::DataType::FLOAT),
            ],
            0,
        ))?;
        assert_eq!(complete(&db, "sel").1, ["SELECT"]);
        assert_eq!(complete(&db, "SELECT * FROM dat").1, ["datasets"]);
        assert_eq!(complete(&db, "SELECT * FROM datasets WHERE si").1, ["size"]);
        assert_eq!(
            complete(&db, "SELECT experiments.ac").1,
            ["experiments.accuracy"]
        );
        drop(db);
        std::fs::remove_dir_all(path)?;
        Ok(())
    }
}
