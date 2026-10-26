// Ввод команд в терминале, история и дополнение по Tab
use minidb::Result;
use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct RawMode(String);
impl RawMode {
    fn new() -> Result<Self> {
        let state = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .output()?;
        if !state.status.success() {
            return Err("Не удалось прочитать настройки терминала".into());
        }
        let guard = Self(String::from_utf8(state.stdout)?.trim().into());
        if !Command::new("stty")
            .args(["-icanon", "-echo", "-isig", "min", "1", "time", "0"])
            .status()?
            .success()
        {
            return Err("Не удалось включить редактирование строки".into());
        }
        Ok(guard)
    }
}
impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = Command::new("stty").arg(&self.0).status();
    }
}

pub enum Input {
    Line(String),
    Interrupt,
    End,
}
#[derive(Default)]
pub struct Editor {
    history: VecDeque<String>,
    path: PathBuf,
}

impl Editor {
    pub fn read_line(
        &mut self,
        reader: &mut impl Read,
        directory: &Path,
        prompt: &str,
        complete: impl Fn(&str) -> (usize, Vec<String>),
    ) -> Result<Input> {
        let path = directory.join(".history");
        if !path.exists() && directory.join(".history.txt").exists() {
            std::fs::rename(directory.join(".history.txt"), &path)?;
        }
        if self.path != path {
            self.history.clear();
            if path.exists() {
                for line in io::BufReader::new(std::fs::File::open(&path)?).lines() {
                    self.history.push_back(line?);
                    if self.history.len() > 1000 {
                        self.history.pop_front();
                    }
                }
            }
            self.path = path;
        }
        let _raw = RawMode::new()?;
        let cols = Command::new("stty")
            .arg("size")
            .stdin(Stdio::inherit())
            .output()?;
        let width = String::from_utf8(cols.stdout)?
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(80);
        let mut screen = Screen {
            width,
            cursor_row: 0,
        };
        let (mut line, mut cursor, mut draft) = (String::new(), 0usize, String::new());
        let mut position = self.history.len();
        screen.draw(prompt, &line, cursor)?;
        loop {
            let Some(key_byte) = byte(reader)? else {
                println!();
                return Ok(Input::End);
            };
            match key_byte {
                b'\r' | b'\n' => {
                    screen.draw(prompt, &line, line.len())?;
                    println!();
                    if !line.trim().is_empty() && self.history.back() != Some(&line) {
                        self.history.push_back(line.clone());
                        if self.history.len() > 1000 {
                            self.history.pop_front();
                        }
                        let history = self
                            .history
                            .iter()
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                            + "\n";
                        std::fs::write(&self.path, history)?;
                    }
                    return Ok(Input::Line(line));
                }
                3 => {
                    screen.draw(prompt, &line, line.len())?;
                    println!("^C");
                    return Ok(Input::Interrupt);
                }
                4 if line.is_empty() => {
                    println!();
                    return Ok(Input::End);
                }
                4 => {
                    if cursor < line.len() {
                        let next = next(&line, cursor);
                        line.replace_range(cursor..next, "");
                    }
                }
                1 => cursor = 0,
                5 => cursor = line.len(),
                127 | 8 if cursor > 0 => {
                    let before = previous(&line, cursor);
                    line.replace_range(before..cursor, "");
                    cursor = before;
                }
                b'\t' => {
                    let (start, candidates) = complete(&line[..cursor]);
                    if let Some(first) = candidates.first() {
                        let mut common = first.clone();
                        for candidate in &candidates[1..] {
                            while !candidate.to_lowercase().starts_with(&common.to_lowercase()) {
                                common.pop();
                            }
                        }
                        if candidates.len() == 1 || common.len() > cursor - start {
                            let mut replacement = common;
                            if candidates.len() == 1 && cursor == line.len() {
                                replacement.push(' ');
                            }
                            line.replace_range(start..cursor, &replacement);
                            cursor = start + replacement.len();
                        } else {
                            screen.draw(prompt, &line, line.len())?;
                            println!("\n{}", candidates.join("  "));
                            screen.cursor_row = 0;
                        }
                    } else {
                        print!("\x07");
                    }
                }
                27 => {
                    let Some(prefix) = byte(reader)? else {
                        continue;
                    };
                    if prefix != b'[' && prefix != b'O' {
                        continue;
                    }
                    let mut key = String::new();
                    while let Some(c) = byte(reader)? {
                        key.push(c as char);
                        if c.is_ascii_alphabetic() || c == b'~' || key.len() > 8 {
                            break;
                        }
                    }
                    match key.as_str() {
                        "A" if position > 0 => {
                            if position == self.history.len() {
                                draft = line.clone();
                            }
                            position -= 1;
                            line = self.history[position].clone();
                            cursor = line.len();
                        }
                        "B" if position < self.history.len() => {
                            position += 1;
                            line = if position == self.history.len() {
                                draft.clone()
                            } else {
                                self.history[position].clone()
                            };
                            cursor = line.len();
                        }
                        "D" if cursor > 0 => cursor = previous(&line, cursor),
                        "C" if cursor < line.len() => cursor = next(&line, cursor),
                        "H" | "1~" | "7~" => cursor = 0,
                        "F" | "4~" | "8~" => cursor = line.len(),
                        "3~" if cursor < line.len() => {
                            let end = next(&line, cursor);
                            line.replace_range(cursor..end, "");
                        }
                        _ => (),
                    }
                }
                c if c >= 32 => {
                    let size = if c < 128 {
                        1
                    } else if c < 224 {
                        2
                    } else if c < 240 {
                        3
                    } else {
                        4
                    };
                    let mut bytes = [0; 4];
                    bytes[0] = c;
                    reader.read_exact(&mut bytes[1..size])?;
                    if let Ok(text) = std::str::from_utf8(&bytes[..size]) {
                        line.insert_str(cursor, text);
                        cursor += text.len();
                    }
                }
                _ => (),
            }
            screen.draw(prompt, &line, cursor)?;
        }
    }
}
fn byte(reader: &mut impl Read) -> io::Result<Option<u8>> {
    let mut byte = [0];
    Ok(if reader.read(&mut byte)? == 0 {
        None
    } else {
        Some(byte[0])
    })
}
fn previous(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(0)
}
fn next(text: &str, cursor: usize) -> usize {
    cursor
        + text[cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(0)
}
fn cells(text: &str) -> usize {
    text.chars()
        .map(|c| match c as u32 {
            0x300..=0x36f => 0,
            0x1100..=0x115f
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0x1f300..=0x1faff => 2,
            _ => 1,
        })
        .sum()
}
struct Screen {
    width: usize,
    cursor_row: usize,
}
impl Screen {
    fn draw(&mut self, prompt: &str, line: &str, cursor: usize) -> io::Result<()> {
        print!("\r");
        if self.cursor_row > 0 {
            print!("\x1b[{}A", self.cursor_row);
        }
        print!("\x1b[J{prompt}{line}");
        let end = cells(prompt) + cells(line);
        let target = cells(prompt) + cells(&line[..cursor]);
        if end > 0 && end.is_multiple_of(self.width) {
            print!(" \x08");
        }
        let up = end / self.width - target / self.width;
        if up > 0 {
            print!("\x1b[{up}A");
        }
        print!("\r");
        if !target.is_multiple_of(self.width) {
            print!("\x1b[{}C", target % self.width);
        }
        self.cursor_row = target / self.width;
        io::stdout().flush()
    }
}
