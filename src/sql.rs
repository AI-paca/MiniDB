// Разбор SQL-команд
use crate::{Column, Compare, DataType, Expr, Kind, Result, Schema, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Word(String),
    Text(String),
    Symbol(String),
}

#[derive(Clone, Debug)]
pub enum Literal {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
}
impl Literal {
    pub fn value(&self) -> Value<'_> {
        match self {
            Self::Null => Value::Null,
            Self::Int(n) => Value::Int(*n),
            Self::Float(n) => Value::Float(*n),
            Self::Text(s) => Value::String(s),
        }
    }
    pub fn for_type(&self, kind: Kind) -> Result<Value<'_>> {
        match (self, kind) {
            (Self::Int(n), Kind::Float { .. }) => Ok(Value::Float(*n as f64)),
            _ => Ok(self.value()),
        }
    }
}

pub enum Condition {
    All,
    Compare(String, Compare, Literal),
    IsNull(String),
    NotNull(String),
    Not(Box<Self>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
}
impl Condition {
    pub fn expression<'a>(&'a self, schema: &Schema) -> Result<Expr<'a>> {
        match self {
            Self::All => Ok(Expr::All),
            Self::NotNull(name) => {
                let name = field_name(schema, name)?;
                schema.column(name)?;
                Ok(Expr::NotNull(name))
            }
            Self::Not(inner) => Ok(Expr::Not(Box::new(inner.expression(schema)?))),
            Self::IsNull(name) => {
                let name = field_name(schema, name)?;
                schema.column(name)?;
                Ok(Expr::IsNull(name))
            }
            Self::Compare(name, op, value) => {
                let name = field_name(schema, name)?;
                Ok(Expr::Compare(
                    name,
                    *op,
                    // Сравнение Int/Float выполняется без округления целого в f64.
                    // Преобразование к типу колонки нужно при записи, а не в WHERE.
                    value.value(),
                ))
            }
            Self::And(a, b) => Ok(a.expression(schema)?.and(b.expression(schema)?)),
            Self::Or(a, b) => Ok(a.expression(schema)?.or(b.expression(schema)?)),
        }
    }
}

fn field_name<'a>(schema: &Schema, name: &'a str) -> Result<&'a str> {
    if let Some((table, field)) = name.split_once('.') {
        if !table.eq_ignore_ascii_case(&schema.name) {
            return Err(format!(
                "WHERE: поле {name} относится к таблице {table}; доступны только поля левой таблицы {}",
                schema.name
            ).into());
        }
        Ok(field)
    } else {
        Ok(name)
    }
}

pub struct Join {
    pub table: String,
    pub left: String,
    pub right: String,
}
pub enum Command {
    Help,
    Tables,
    DatabasePath,
    Describe(String),
    Open(String),
    Backup(String),
    Restore(String, String),
    Exit,
    Create(Schema),
    AddColumn {
        table: String,
        column: Column,
    },
    Insert {
        table: String,
        columns: Vec<String>,
        values: Vec<Literal>,
    },
    Select {
        table: String,
        columns: Vec<String>,
        join: Option<Join>,
        condition: Condition,
    },
    Update {
        table: String,
        values: Vec<(String, Literal)>,
        condition: Condition,
    },
    Delete {
        table: String,
        condition: Condition,
    },
}

// Разделение команды на слова, строки и операторы
pub fn tokenize(text: &str) -> Result<Vec<Token>> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '-' && chars.peek() == Some(&'-') {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            continue;
        }
        if c == '\'' || c == '"' {
            let mut value = String::new();
            let mut closed = false;
            while let Some(next) = chars.next() {
                if next == c {
                    if chars.peek() == Some(&c) {
                        chars.next();
                        value.push(c);
                    } else {
                        closed = true;
                        break;
                    }
                } else if next == '\\' {
                    value.push(match chars.next().ok_or("Оборванное экранирование")?
                    {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '0' => '\0',
                        'u' => {
                            if chars.next() != Some('{') {
                                return Err("Ожидалось \\u{hex}".into());
                            }
                            let mut hex = String::new();
                            loop {
                                match chars.next() {
                                    Some('}') => break,
                                    Some(c) if c.is_ascii_hexdigit() && hex.len() < 6 => {
                                        hex.push(c)
                                    }
                                    _ => return Err("Повреждён Unicode escape".into()),
                                }
                            }
                            char::from_u32(u32::from_str_radix(&hex, 16)?)
                                .ok_or("Некорректный Unicode символ")?
                        }
                        '\\' => '\\',
                        '\'' => '\'',
                        '"' => '"',
                        _ => return Err("Допустимы \\n, \\r, \\t, \\\\, \\' и \\\"".into()),
                    });
                } else {
                    value.push(next);
                }
            }
            if !closed {
                return Err("Не закрыты кавычки".into());
            }
            tokens.push(Token::Text(value));
        } else if "(),;*=<>!".contains(c) {
            let mut symbol = c.to_string();
            if "<>!".contains(c) && chars.peek() == Some(&'=') {
                symbol.push(chars.next().ok_or("Оборванный оператор")?);
            }
            if symbol == "!" {
                return Err("Используйте !=".into());
            }
            tokens.push(Token::Symbol(symbol));
        } else {
            let mut word = c.to_string();
            while let Some(&next) = chars.peek() {
                if next.is_whitespace() || "(),;*=<>!'\"".contains(next) {
                    break;
                }
                word.push(chars.next().ok_or("Оборванное слово")?);
            }
            tokens.push(Token::Word(word));
        }
    }
    Ok(tokens)
}

pub fn parse(text: &str) -> Result<Vec<Command>> {
    if text
        .trim()
        .trim_end_matches(';')
        .trim()
        .eq_ignore_ascii_case("--help")
    {
        return Ok(vec![Command::Help]);
    }
    let mut parser = Parser::new(text, true)?;
    let mut commands = Vec::new();
    while parser.pos < parser.tokens.len() {
        if parser.eat(";") {
            continue;
        }
        commands.push(parser.command()?);
        if parser.pos < parser.tokens.len() {
            parser.need(";")?;
        }
    }
    Ok(commands)
}

pub(crate) fn stored_schema(text: &str) -> Result<Schema> {
    let mut parser = Parser::new(text, false)?;
    parser.need("CREATE")?;
    let mut schema = parser.schema(true)?;
    parser.eat(";");
    let mut seen_version = false;
    while parser.pos < parser.tokens.len() {
        if parser.eat("VERSION") {
            if seen_version {
                return Err("Повтор VERSION".into());
            }
            seen_version = true;
            schema.version = parser
                .word()?
                .parse()
                .map_err(|_| "VERSION: ожидается u32")?;
        } else if parser.eat("COLUMNS_AT_VERSION") {
            let version = parser
                .word()?
                .parse()
                .map_err(|_| "COLUMNS_AT_VERSION: ожидается версия u32")?;
            parser.need("=")?;
            let count = parser
                .word()?
                .parse()
                .map_err(|_| "COLUMNS_AT_VERSION: ожидается число колонок")?;
            if schema.columns_at_version.insert(version, count).is_some() {
                return Err("Повтор COLUMNS_AT_VERSION".into());
            }
        } else {
            return Err("Лишний текст схемы".into());
        }
    }
    Ok(schema)
}

struct Parser {
    tokens: Vec<Token>,           // Исходные имена, строки и пути.
    grammar: Vec<Option<String>>, // Отдельная UPPERCASE-копия только слов/символов.
    pos: usize,
    fold_names: bool,
}
impl Parser {
    fn new(text: &str, fold_names: bool) -> Result<Self> {
        let tokens = tokenize(text)?;
        let grammar = tokens
            .iter()
            .map(|token| match token {
                Token::Word(word) | Token::Symbol(word) => Some(word.to_ascii_uppercase()),
                Token::Text(_) => None,
            })
            .collect();
        Ok(Self {
            tokens,
            grammar,
            pos: 0,
            fold_names,
        })
    }
    fn keyword(&mut self) -> Result<String> {
        if !matches!(self.tokens.get(self.pos), Some(Token::Word(_))) {
            return Err("Ожидалось ключевое слово".into());
        }
        let word = self.grammar[self.pos]
            .clone()
            .ok_or("Ожидалось ключевое слово")?;
        self.pos += 1;
        Ok(word)
    }
    fn eat(&mut self, expected: &str) -> bool {
        let matched = self
            .grammar
            .get(self.pos)
            .and_then(Option::as_deref)
            .is_some_and(|word| word == expected.to_ascii_uppercase());
        if matched {
            self.pos += 1;
        }
        matched
    }
    fn need(&mut self, expected: &str) -> Result<()> {
        if self.eat(expected) {
            Ok(())
        } else {
            Err(format!(
                "Ожидалось {expected}; получено {}.",
                self.tokens
                    .get(self.pos)
                    .map(|token| match token {
                        Token::Word(text) | Token::Symbol(text) => text.clone(),
                        Token::Text(text) => format!("'{text}'"),
                    })
                    .unwrap_or_else(|| "конец команды".into())
            )
            .into())
        }
    }
    fn word(&mut self) -> Result<String> {
        match self.tokens.get(self.pos) {
            Some(Token::Word(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => Err("Ожидалось имя".into()),
        }
    }
    fn name(&mut self) -> Result<String> {
        let word = self.word()?;
        Ok(if self.fold_names {
            word.to_lowercase()
        } else {
            word
        })
    }
    fn path(&mut self) -> Result<String> {
        match self.tokens.get(self.pos) {
            Some(Token::Text(s) | Token::Word(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => Err("Укажите путь, например 'data/my-db'".into()),
        }
    }
    fn literal(&mut self) -> Result<Literal> {
        match self.tokens.get(self.pos).cloned() {
            Some(Token::Text(s)) => {
                self.pos += 1;
                Ok(Literal::Text(s))
            }
            Some(Token::Word(s)) => {
                self.pos += 1;
                if s.eq_ignore_ascii_case("null") {
                    return Ok(Literal::Null);
                }
                if let Ok(n) = s.parse::<i64>() {
                    return Ok(Literal::Int(n));
                }
                let n: f64 = s
                    .parse()
                    .map_err(|_| "Строки заключаются в кавычки: 'Alice'")?;
                if !n.is_finite() {
                    return Err("FLOAT должен быть конечным".into());
                }
                Ok(Literal::Float(n))
            }
            _ => Err("Ожидалось число, NULL или строка в кавычках".into()),
        }
    }
    fn command(&mut self) -> Result<Command> {
        let command = self.keyword()?;
        Ok(match command.as_str() {
            "HELP" | "-H" | "--HELP" | ".HELP" => Command::Help,
            "EXIT" | "QUIT" | ".EXIT" => Command::Exit,
            "TABLES" => Command::Tables,
            "SHOW" => {
                if self.eat("DATABASE") {
                    Command::DatabasePath
                } else {
                    self.need("TABLES")?;
                    Command::Tables
                }
            }
            "DESCRIBE" | "DESC" => Command::Describe(self.name()?),
            "OPEN" => Command::Open(self.path()?),
            "BACKUP" => Command::Backup(self.path()?),
            "RESTORE" => {
                let backup = self.path()?;
                self.need("TO")?;
                Command::Restore(backup, self.path()?)
            }
            "CREATE" => {
                let schema = self.schema(false)?;
                Command::Create(schema)
            }
            "ALTER" => {
                self.need("TABLE")?;
                let table = self.name()?;
                if self.eat("DROP") {
                    return Err("Удаление столбца (DROP COLUMN) пока не поддерживается; DELETE удаляет строки, а не столбцы".into());
                }
                self.need("ADD")?;
                self.eat("COLUMN");
                let (column, primary_key) = self.column(true)?;
                if !column.data_type.nullable {
                    return Err("ADD COLUMN требует NULL: старые записи не имеют значения".into());
                }
                if primary_key {
                    return Err("ALTER ADD COLUMN не добавляет второй PRIMARY KEY".into());
                }
                Command::AddColumn { table, column }
            }
            "INSERT" => {
                self.need("INTO")?;
                let table = self.name()?;
                let columns = if self.eat("(") {
                    self.names(")")?
                } else {
                    Vec::new()
                };
                self.need("VALUES")?;
                self.need("(")?;
                let mut values = Vec::new();
                loop {
                    values.push(self.literal()?);
                    if self.eat(")") {
                        break;
                    }
                    self.need(",")?;
                }
                Command::Insert {
                    table,
                    columns,
                    values,
                }
            }
            "SELECT" => {
                let columns = if self.eat("*") {
                    Vec::new()
                } else {
                    let mut columns = vec![self.name()?];
                    while self.eat(",") {
                        columns.push(self.name()?);
                    }
                    columns
                };
                self.need("FROM")?;
                let table = self.name()?;
                let join = if self.eat("INNER") {
                    self.need("JOIN")?;
                    Some(self.join()?)
                } else if self.eat("JOIN") {
                    Some(self.join()?)
                } else {
                    None
                };
                let condition = self.condition()?;
                Command::Select {
                    table,
                    columns,
                    join,
                    condition,
                }
            }
            "UPDATE" => {
                let table = self.name()?;
                self.need("SET")?;
                let mut values = Vec::new();
                loop {
                    let name = self.name()?;
                    self.need("=")?;
                    values.push((name, self.literal()?));
                    if !self.eat(",") {
                        break;
                    }
                }
                Command::Update {
                    table,
                    values,
                    condition: self.condition()?,
                }
            }
            "DELETE" => {
                self.need("FROM")?;
                Command::Delete {
                    table: self.name()?,
                    condition: self.condition()?,
                }
            }
            _ => return Err(format!("Неизвестная команда {command}.").into()),
        })
    }
    fn names(&mut self, end: &str) -> Result<Vec<String>> {
        let mut names = Vec::new();
        loop {
            names.push(self.name()?);
            if self.eat(end) {
                break;
            }
            self.need(",")?;
        }
        Ok(names)
    }
    fn join(&mut self) -> Result<Join> {
        let table = self.name()?;
        self.need("on")?;
        let left = self.name()?;
        self.need("=")?;
        let right = self.name()?;
        Ok(Join { table, left, right })
    }
    fn condition(&mut self) -> Result<Condition> {
        if self.eat("where") {
            self.or(0)
        } else {
            Ok(Condition::All)
        }
    }
    fn or(&mut self, depth: usize) -> Result<Condition> {
        let mut condition = self.and(depth)?;
        while self.eat("or") {
            condition = Condition::Or(Box::new(condition), Box::new(self.and(depth)?));
        }
        Ok(condition)
    }
    fn and(&mut self, depth: usize) -> Result<Condition> {
        let mut condition = self.atom(depth)?;
        while self.eat("and") {
            condition = Condition::And(Box::new(condition), Box::new(self.atom(depth)?));
        }
        Ok(condition)
    }
    fn atom(&mut self, depth: usize) -> Result<Condition> {
        if depth > 64 {
            return Err("Условие вложено слишком глубоко".into());
        }
        if self.eat("not") {
            return Ok(Condition::Not(Box::new(self.atom(depth + 1)?)));
        }
        if self.eat("(") {
            let condition = self.or(depth + 1)?;
            self.need(")")?;
            return Ok(condition);
        }
        let name = self.name()?;
        if self.eat("is") {
            let negated = self.eat("not");
            self.need("null")?;
            return Ok(if negated {
                Condition::NotNull(name)
            } else {
                Condition::IsNull(name)
            });
        }
        let op = match self.word_or_symbol()?.as_str() {
            "=" => Compare::Eq,
            "!=" => Compare::Ne,
            ">" => Compare::Gt,
            "<" => Compare::Lt,
            ">=" => Compare::Ge,
            "<=" => Compare::Le,
            _ => return Err("Операторы сравнения: = != > < >= <=".into()),
        };
        Ok(Condition::Compare(name, op, self.literal()?))
    }
    fn word_or_symbol(&mut self) -> Result<String> {
        match self.tokens.get(self.pos) {
            Some(Token::Word(s) | Token::Symbol(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => Err("Ожидался оператор".into()),
        }
    }
    // CREATE и ALTER используют один разбор определения столбца.
    fn column(&mut self, allow_inferred: bool) -> Result<(Column, bool)> {
        let column_name = self.name()?;
        let omitted = allow_inferred
            && (self.pos == self.tokens.len() || self.grammar[self.pos].as_deref() == Some(";"));
        let mut data_type =
            if omitted {
                DataType::NULL
            } else {
                match self.keyword()?.as_str() {
                    "INT" => DataType::INT,
                    "FLOAT" => DataType::FLOAT,
                    "STRING" => DataType::STRING,
                    "NULL" => DataType::NULL,
                    _ => return Err(
                        "Типы: INT, FLOAT, STRING; новый столбец без типа ожидает первого значения"
                            .into(),
                    ),
                }
            };
        if self.eat("(") {
            let max_bytes = self.word()?.parse()?;
            self.need(")")?;
            if !matches!(data_type.kind, Kind::String { .. }) {
                return Err("Длина задаётся только для STRING".into());
            }
            data_type.kind = Kind::String { max_bytes };
        }
        // Обычные поля допускают NULL, первичный ключ всегда заполнен
        data_type.nullable = true;
        let mut references = None;
        let mut pk = false;
        loop {
            if self.eat("primary") {
                self.need("key")?;
                pk = true;
            } else if self.eat("not") {
                self.need("null")?;
                data_type.nullable = false;
            } else if self.eat("null") {
                data_type.nullable = true;
            } else if self.eat("references") {
                let table = self.name()?;
                self.need("(")?;
                let column = self.name()?;
                self.need(")")?;
                references = Some(crate::Reference { table, column });
            } else if self.eat("min") {
                let bound = self.literal()?;
                match (&mut data_type.kind, bound) {
                    (Kind::Int { min, .. }, Literal::Int(n)) => *min = n,
                    (Kind::Float { min, .. }, Literal::Float(n)) => *min = n,
                    (Kind::Float { min, .. }, Literal::Int(n)) => *min = n as f64,
                    _ => return Err("MIN требует число соответствующего типа".into()),
                }
            } else if self.eat("max") {
                let bound = self.literal()?;
                match (&mut data_type.kind, bound) {
                    (Kind::Int { max, .. }, Literal::Int(n)) => *max = n,
                    (Kind::Float { max, .. }, Literal::Float(n)) => *max = n,
                    (Kind::Float { max, .. }, Literal::Int(n)) => *max = n as f64,
                    _ => return Err("MAX требует число соответствующего типа".into()),
                }
            } else {
                break;
            }
        }
        if pk {
            data_type.nullable = false;
        }
        data_type.check()?;
        Ok((
            Column {
                name: column_name,
                data_type,
                references,
            },
            pk,
        ))
    }
    fn schema(&mut self, stored: bool) -> Result<Schema> {
        self.need("table")?;
        let name = self.name()?;
        self.need("(")?;
        let mut columns = Vec::new();
        let mut primary_key = None;
        loop {
            let (column, pk) = self.column(false)?;
            if pk && primary_key.replace(columns.len()).is_some() {
                return Err("Разрешён один PRIMARY KEY".into());
            }
            columns.push(column);
            if self.eat(")") {
                break;
            }
            self.need(",")?;
        }
        let mut schema = Schema::new(&name, columns, primary_key.ok_or("Укажите PRIMARY KEY")?);
        if self.eat("validator") {
            if !stored {
                return Err("VALIDATOR не входит в SQL-команды CLI; используйте сохранённую Schema и библиотечный Database::open_with с Rust-функцией".into());
            }
            schema.validator = Some(self.name()?);
        }
        if !stored {
            schema.check()?;
        }
        Ok(schema)
    }
}
