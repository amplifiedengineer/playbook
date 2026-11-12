// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! A small, quote-aware reader for shell command lines, shared by the hooks
//! that need to see which program a Bash call runs and with what words.
//!
//! It splits on unquoted separators into simple commands, removes quotes and
//! keeps where each word sits in the original text, so a caller can rewrite
//! one word in place. Each command also keeps the heredoc bodies opened on its
//! line. It does not expand variables, aliases, functions or substitutions,
//! so every caller is a guardrail against drift rather than a security
//! boundary.

use std::ops::Range;
use std::path::Path;

const SHELL_KEYWORDS: [&str; 10] = [
    "if", "then", "else", "elif", "while", "until", "do", "!", "{", "}",
];
const WRAPPERS: [&str; 7] = ["env", "command", "exec", "time", "nice", "timeout", "sudo"];

/// Shells that run the script after `-c`, or read one from standard input.
pub const SHELLS: [&str; 5] = ["sh", "bash", "zsh", "dash", "ksh"];

/// A program that runs another command given as its remaining words.
pub struct Runner {
    pub name: &'static str,
    /// The options that take the next word as their value.
    pub value_options: &'static [&'static str],
}

const fn runner(name: &'static str, value_options: &'static [&'static str]) -> Runner {
    Runner {
        name,
        value_options,
    }
}

pub const RUNNERS: &[Runner] = &[
    runner("nohup", &[]),
    runner("xargs", &["-I", "-L", "-n", "-P", "-d", "-E", "-s", "-a"]),
    runner("setsid", &[]),
    runner("stdbuf", &["-i", "-o", "-e"]),
    runner("watch", &["-n", "-d"]),
    runner("parallel", &["-j", "-S"]),
    runner("ionice", &["-c", "-n", "-p", "-P", "-u"]),
    runner("caffeinate", &["-t", "-w"]),
    runner("unbuffer", &[]),
    runner("doas", &["-u", "-C"]),
    runner("chronic", &[]),
    runner("busybox", &[]),
];

/// The options `program` takes a value for, when it is a runner.
pub fn runner_value_options(program: &str) -> Option<&'static [&'static str]> {
    RUNNERS
        .iter()
        .find(|r| r.name == program)
        .map(|r| r.value_options)
}

/// The index of the first word from `from` that is not an option, skipping
/// the value of each option in `takes_value` and stopping after `--`.
pub fn skip_options(words: &[String], from: usize, takes_value: &[&str]) -> usize {
    let mut next = from;
    while let Some(word) = words.get(next) {
        if takes_value.contains(&word.as_str()) {
            next += 2;
        } else if word == "--" {
            next += 1;
            break;
        } else if word.len() > 1 && word.starts_with('-') {
            next += 1;
        } else {
            break;
        }
    }
    next.min(words.len())
}

/// The index in `args` of the script after a shell's `-c`, which may sit in
/// a cluster such as `-lc`.
pub fn shell_script_at(args: &[String]) -> Option<usize> {
    let at = args
        .iter()
        .position(|w| w.starts_with('-') && !w.starts_with("--") && w.contains('c'))?;
    (at + 1 < args.len()).then_some(at + 1)
}

/// The index of the word that names the program: past leading assignments,
/// redirections, shell keywords and wrappers such as `env` or `sudo`.
pub fn program_index(words: &[String]) -> Option<usize> {
    let mut at = 0;
    while let Some(word) = words.get(at) {
        let redirect = redirect_len(word);
        if redirect > 0 {
            at += redirect;
        } else if is_assignment(word) || SHELL_KEYWORDS.contains(&word.as_str()) {
            at += 1;
        } else if WRAPPERS.contains(&program_name(word).as_str()) {
            at = after_wrapper(words, at);
        } else {
            return Some(at);
        }
    }
    None
}

/// The index just past the wrapper at `at` and its own options. Always moves
/// forward.
fn after_wrapper(words: &[String], at: usize) -> usize {
    let wrapper = program_name(&words[at]);
    let takes_value: &[&str] = match wrapper.as_str() {
        "env" => &["-u", "-C", "--unset", "--chdir"],
        "exec" => &["-a"],
        "nice" => &["-n", "--adjustment"],
        "timeout" => &["-s", "-k", "--signal", "--kill-after"],
        "sudo" => &[
            "-u", "-g", "-h", "-p", "-C", "-D", "-R", "-T", "-r", "-t", "-U",
        ],
        _ => &[],
    };
    let mut next = at + 1;
    while let Some(word) = words.get(next) {
        if takes_value.contains(&word.as_str()) {
            next += 2;
        } else if word == "--" {
            next += 1;
            break;
        } else if (word.len() > 1 && word.starts_with('-'))
            || (wrapper == "env" && is_assignment(word))
        {
            next += 1;
        } else {
            break;
        }
    }
    if wrapper == "timeout" {
        next += 1;
    }
    next.min(words.len())
}

/// The words a redirection takes: the operator word, plus its target when
/// the operator stands alone.
pub fn redirect_len(word: &str) -> usize {
    let operator = word.trim_start_matches(|c: char| c.is_ascii_digit());
    if !operator.starts_with(['<', '>']) {
        0
    } else if operator.chars().all(|c| c == '<' || c == '>') {
        2
    } else {
        1
    }
}

/// The lowercase file name of `word`, so `/usr/bin/Playbook` is `playbook`.
pub fn program_name(word: &str) -> String {
    Path::new(word)
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().to_lowercase())
}

pub fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Where a word sits in the text it was read from, in characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    /// The word holds `$` or a backtick outside single quotes, so its value is
    /// only known to the shell that runs it.
    pub dynamic: bool,
}

/// A heredoc body opened on a command's line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heredoc {
    pub delimiter: String,
    /// The body is not expanded, because the delimiter was quoted.
    pub quoted: bool,
    /// The operator was `<<-`, so leading tabs of the body and of the
    /// delimiter line are ignored.
    pub strip_tabs: bool,
    /// The body's lines, with leading tabs removed for `<<-`.
    pub text: String,
    /// The body's lines in the source, up to the delimiter line.
    pub body: Range<usize>,
}

/// One simple command.
#[derive(Debug)]
pub struct Command {
    pub words: Vec<String>,
    /// One span per word.
    pub spans: Vec<Span>,
    pub heredocs: Vec<Heredoc>,
    /// The command reads the output of the one before it, as after `|`.
    pub piped: bool,
    /// How many parentheses, as in `( ... )` or `$( ... )`, enclose the
    /// command: what it changes, such as the directory, ends with them.
    pub depth: usize,
    /// Which innermost parenthesis group the command is in, numbered from 1 in
    /// the order they open, or 0 outside any. Sibling groups differ.
    pub group: usize,
    /// The line break, as a character index, that ends the logical line the
    /// command sits on: the first one outside quotes, substitutions and line
    /// continuations after it. A heredoc the command opens has its body right
    /// after it. `None` when the script ends first.
    pub line_end: Option<usize>,
    /// The command runs inside a command substitution, `$( ... )` or a
    /// backtick pair, so its output becomes words of another command.
    pub substitution: bool,
    /// The command carries on a command that a substitution split, as the
    /// `rm` in `X=$(pwd) rm a`: no separator stands between them.
    pub joined: bool,
    /// The command ends where a substitution opens, so the substitution's
    /// output becomes more of its words, as in `rm `cat list``.
    pub opens_substitution: bool,
}

/// The words of each simple command in `command`. Heredoc bodies and comments
/// are dropped, since neither is run by this shell.
pub fn simple_commands(command: &str) -> Vec<Vec<String>> {
    commands(command)
        .into_iter()
        .map(|command| command.words)
        .collect()
}

/// Splits `command` on unquoted separators into simple commands. A heredoc
/// body belongs to the command whose line opened it, which for
/// `cat <<EOF | git commit -F -` is `cat`, not `git`. A heredoc opened on a
/// line with no command words belongs to no command.
pub fn commands(command: &str) -> Vec<Command> {
    Lexer::new(command).run()
}

/// The command texts of the `$( ... )` groups and backtick pairs in `text`,
/// read the way the shell reads them: quotes, comments and heredoc bodies
/// inside a group do not end it. With `quotes`, single-quoted text is skipped,
/// as in a word. Without, quotes are literal, as in an unquoted heredoc body.
pub fn substitutions(text: &str, quotes: bool) -> Vec<String> {
    let mut lexer = Lexer::new(text);
    let mut found = Vec::new();
    let mut in_double = false;
    while let Some(c) = lexer.take() {
        match c {
            '\\' => {
                lexer.take();
            }
            '"' if quotes => in_double = !in_double,
            '\'' if quotes && !in_double => while lexer.take().is_some_and(|c| c != '\'') {},
            '$' if lexer.looking_at("((") => {
                let arithmetic = lexer.substitution();
                found.extend(substitutions(&arithmetic, quotes));
            }
            '$' if lexer.looking_at("(") => {
                let group = lexer.substitution();
                let inner = group.strip_prefix('(').unwrap_or(&group);
                found.push(inner.strip_suffix(')').unwrap_or(inner).to_string());
            }
            '`' => {
                let mut inner = String::new();
                // Inside backticks a backslash only escapes `$`, a backtick
                // or another backslash; before anything else it stays.
                while let Some(c) = lexer.take().filter(|&c| c != '`') {
                    match (c, lexer.peek()) {
                        ('\\', Some(next @ ('$' | '`' | '\\'))) => {
                            lexer.take();
                            inner.push(next);
                        }
                        _ => inner.push(c),
                    }
                }
                found.push(inner);
            }
            _ => {}
        }
    }
    found
}

impl Lexer {
    fn new(command: &str) -> Lexer {
        Lexer {
            chars: command.chars().collect(),
            at: 0,
            commands: Vec::new(),
            words: Vec::new(),
            spans: Vec::new(),
            word: None,
            word_start: 0,
            word_end: 0,
            word_dynamic: false,
            opened: Vec::new(),
            opened_here: Vec::new(),
            piped_next: false,
            groups: Vec::new(),
            groups_opened: 0,
            substitution_groups: Vec::new(),
            in_backtick: false,
            joined_next: false,
            opens_next: false,
            line_first: 0,
        }
    }
}

/// Replaces the character ranges of `text` named by `edits`. A range that
/// overlaps an earlier one, runs backwards or leaves the text is skipped.
pub fn apply_edits(text: &str, edits: &mut [(Range<usize>, String)]) -> String {
    edits.sort_by_key(|(range, _)| range.start);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    for (range, replacement) in edits.iter() {
        if range.start < at || range.start > range.end || range.end > chars.len() {
            continue;
        }
        out.extend(&chars[at..range.start]);
        out.push_str(replacement);
        at = range.end;
    }
    out.extend(&chars[at..]);
    out
}

/// `text` as one shell word that reads back as exactly `text`.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `text` as one shell word, bare when every character is safe, else quoted.
pub fn word(text: &str) -> String {
    let plain = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-:@%+=".contains(c));
    if plain && !text.is_empty() {
        text.to_string()
    } else {
        quote(text)
    }
}

/// Resolves backslash escapes the way `$'...'`, `printf` and `echo -e` do.
/// An unknown escape is kept as written.
pub fn unescape(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        if chars[at] != '\\' || at + 1 == chars.len() {
            out.push(chars[at]);
            at += 1;
            continue;
        }
        let escape = chars[at + 1];
        at += 2;
        let simple = match escape {
            'a' => Some('\u{7}'),
            'b' => Some('\u{8}'),
            'e' | 'E' => Some('\u{1b}'),
            'f' => Some('\u{c}'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'v' => Some('\u{b}'),
            '\\' | '\'' | '"' | '?' => Some(escape),
            _ => None,
        };
        if let Some(c) = simple {
            out.push(c);
            continue;
        }
        let (radix, max) = match escape {
            'x' => (16, 2),
            'u' => (16, 4),
            'U' => (16, 8),
            '0'..='7' => {
                at -= 1;
                (8, 3)
            }
            _ => {
                out.push('\\');
                out.push(escape);
                continue;
            }
        };
        let digits: String = chars[at..]
            .iter()
            .take(max)
            .take_while(|c| c.is_digit(radix))
            .collect();
        match u32::from_str_radix(&digits, radix)
            .ok()
            .and_then(char::from_u32)
        {
            Some(c) => {
                out.push(c);
                at += digits.len();
            }
            None => {
                out.push('\\');
                out.push(escape);
            }
        }
    }
    out
}

/// A heredoc operator read on the current line, whose body starts at its end.
struct Opened {
    delimiter: String,
    strip_tabs: bool,
    quoted: bool,
    /// Index the opening command takes in `Lexer::commands`, once it is known.
    owner: Option<usize>,
}

struct Lexer {
    chars: Vec<char>,
    at: usize,
    commands: Vec<Command>,
    words: Vec<String>,
    spans: Vec<Span>,
    /// The word being read. `Some("")` after an empty quoted string.
    word: Option<String>,
    word_start: usize,
    word_end: usize,
    word_dynamic: bool,
    /// Heredocs opened on the current line, whose bodies start at its end.
    opened: Vec<Opened>,
    /// The ones in `opened` that belong to the command being read.
    opened_here: Vec<usize>,
    /// The next command reads the output of the one before it.
    piped_next: bool,
    /// The ids of the parentheses open around the position being read.
    groups: Vec<usize>,
    groups_opened: usize,
    /// One entry per open parenthesis: whether it opened a `$( ... )`.
    substitution_groups: Vec<bool>,
    /// Inside an unquoted backtick pair.
    in_backtick: bool,
    /// The next command carries on one a substitution split.
    joined_next: bool,
    /// The command being ended stops where a substitution opens.
    opens_next: bool,
    /// Index in `commands` of the first command on the current line.
    line_first: usize,
}

impl Lexer {
    fn run(mut self) -> Vec<Command> {
        while let Some(c) = self.peek() {
            let start = self.at;
            self.at += 1;
            match c {
                '\'' => {
                    self.begin(start);
                    self.single_quoted();
                }
                '"' => {
                    self.begin(start);
                    self.double_quoted();
                }
                '\\' => self.escaped(start),
                '$' if self.looking_at("'") => {
                    self.begin(start);
                    self.at += 1;
                    self.ansi_quoted();
                }
                '$' if self.looking_at("\"") => {
                    self.begin(start);
                    self.at += 1;
                    self.double_quoted();
                }
                '$' if self.looking_at("((") => {
                    self.begin(start);
                    self.word_dynamic = true;
                    let text = self.substitution();
                    self.push_str(&format!("${text}"));
                }
                '$' => {
                    self.begin(start);
                    self.word_dynamic = true;
                    self.push_str("$");
                }
                '#' if self.word.is_none() => self.skip_comment(),
                '<' if self.looking_at("<<") => {
                    self.end_word();
                    self.begin(start);
                    self.at += 2;
                    self.push_str("<<<");
                    self.word_end = self.at;
                    self.end_word();
                }
                '<' if self.looking_at("<") => {
                    self.end_word();
                    self.heredoc();
                }
                '|' => {
                    let or_list =
                        self.looking_at("|") || (self.at >= 2 && self.chars[self.at - 2] == '|');
                    if self.looking_at("&") {
                        self.at += 1;
                    }
                    self.end_command();
                    self.piped_next = !or_list;
                    self.joined_next = false;
                }
                '`' | ';' | '&' | '(' | ')' => {
                    // A word that runs into a backtick stays dynamic either way:
                    // the commit message sanitizer must not rewrite text that
                    // sits inside a substitution.
                    if c == '`' && self.word.is_some() {
                        self.word_dynamic = true;
                    }
                    let opens = match c {
                        '(' => start > 0 && self.chars[start - 1] == '$',
                        '`' => !self.in_backtick,
                        _ => false,
                    };
                    self.opens_next = opens;
                    self.end_command();
                    self.piped_next = false;
                    self.joined_next = false;
                    match c {
                        '(' => {
                            self.groups_opened += 1;
                            self.groups.push(self.groups_opened);
                            self.substitution_groups.push(opens);
                        }
                        ')' => {
                            self.groups.pop();
                            self.joined_next = self.substitution_groups.pop().unwrap_or(false);
                        }
                        '`' => {
                            self.in_backtick = !self.in_backtick;
                            self.joined_next = !self.in_backtick;
                        }
                        _ => {}
                    }
                }
                '\n' | '\r' => {
                    self.end_command();
                    self.joined_next = false;
                    if c == '\n' {
                        for command in &mut self.commands[self.line_first..] {
                            command.line_end = Some(start);
                        }
                        self.line_first = self.commands.len();
                        self.read_heredoc_bodies();
                    }
                }
                c if c.is_whitespace() => self.end_word(),
                c => {
                    self.begin(start);
                    self.push_str(c.encode_utf8(&mut [0; 4]));
                }
            }
            if self.word.is_some() {
                self.word_end = self.at;
            }
        }
        self.end_command();
        self.commands
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn take(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.at += 1;
        }
        c
    }

    fn looking_at(&self, text: &str) -> bool {
        text.chars()
            .enumerate()
            .all(|(i, c)| self.chars.get(self.at + i) == Some(&c))
    }

    /// Starts a word at `start` unless one is already being read.
    fn begin(&mut self, start: usize) {
        if self.word.is_none() {
            self.word = Some(String::new());
            self.word_start = start;
            self.word_dynamic = false;
        }
    }

    fn push_str(&mut self, text: &str) {
        self.word.get_or_insert_with(String::new).push_str(text);
    }

    fn end_word(&mut self) {
        if let Some(word) = self.word.take() {
            self.words.push(word);
            self.spans.push(Span {
                start: self.word_start,
                end: self.word_end.min(self.chars.len()),
                dynamic: self.word_dynamic,
            });
        }
    }

    fn end_command(&mut self) {
        self.end_word();
        let here = std::mem::take(&mut self.opened_here);
        let opens_substitution = std::mem::take(&mut self.opens_next);
        if self.words.is_empty() {
            return;
        }
        let index = self.commands.len();
        for at in here {
            self.opened[at].owner = Some(index);
        }
        self.commands.push(Command {
            words: std::mem::take(&mut self.words),
            spans: std::mem::take(&mut self.spans),
            heredocs: Vec::new(),
            piped: std::mem::take(&mut self.piped_next),
            depth: self.groups.len(),
            group: self.groups.last().copied().unwrap_or(0),
            line_end: None,
            substitution: self.in_backtick || self.substitution_groups.contains(&true),
            joined: std::mem::take(&mut self.joined_next),
            opens_substitution,
        });
    }

    fn single_quoted(&mut self) {
        let mut text = String::new();
        while let Some(c) = self.take().filter(|&c| c != '\'') {
            text.push(c);
        }
        self.push_str(&text);
    }

    /// Inside double quotes a backslash only escapes `$`, a backtick, `"`,
    /// another backslash and a line break, and `$` or a backtick still expand.
    fn double_quoted(&mut self) {
        let mut text = String::new();
        while let Some(c) = self.take().filter(|&c| c != '"') {
            match c {
                '\\' => match self.peek() {
                    Some(next @ ('$' | '`' | '"' | '\\')) => {
                        self.at += 1;
                        text.push(next);
                    }
                    Some('\n') => self.at += 1,
                    _ => text.push('\\'),
                },
                '$' if self.looking_at("(") => {
                    self.word_dynamic = true;
                    text.push('$');
                    text.push_str(&self.substitution());
                }
                '$' | '`' => {
                    self.word_dynamic = true;
                    text.push(c);
                }
                c => text.push(c),
            }
        }
        self.push_str(&text);
    }

    /// The text of a `$(...)` or `$((...))` from its opening parenthesis to the
    /// matching one, which `at` stands on. Quotes, comments and heredoc bodies
    /// inside it are skipped over, so a `"` or `)` there does not end it.
    /// Inside `$((...))` only parentheses count, as `<<` there is a shift.
    fn substitution(&mut self) -> String {
        let arithmetic = self.looking_at("((");
        let mut text = String::new();
        let mut depth = 0;
        let mut heredocs: Vec<(String, bool)> = Vec::new();
        while let Some(c) = self.take() {
            text.push(c);
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ if arithmetic => {}
                '\\' => text.extend(self.take()),
                '\'' => {
                    while let Some(inner) = self.take() {
                        text.push(inner);
                        if inner == '\'' {
                            break;
                        }
                    }
                }
                '"' => self.skip_double_quoted(&mut text),
                '#' if self.at < 2 || self.chars[self.at - 2].is_whitespace() => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        text.extend(self.take());
                    }
                }
                '<' if self.looking_at("<") && !self.looking_at("<<") => {
                    let strip_tabs = self.looking_at("<-");
                    let from = self.at;
                    self.at += 1 + usize::from(strip_tabs);
                    while self.looking_at(" ") || self.looking_at("\t") {
                        self.at += 1;
                    }
                    let (delimiter, _) = self.delimiter();
                    text.extend(self.chars[from..self.at].iter());
                    heredocs.push((delimiter, strip_tabs));
                }
                '\n' => {
                    for (delimiter, strip_tabs) in heredocs.drain(..) {
                        self.skip_heredoc_body(&delimiter, strip_tabs, &mut text);
                    }
                }
                _ => {}
            }
        }
        text
    }

    /// Copies the rest of a double-quoted string, nested substitutions
    /// included, onto `text`.
    fn skip_double_quoted(&mut self, text: &mut String) {
        while let Some(c) = self.take() {
            text.push(c);
            match c {
                '"' => return,
                '\\' => text.extend(self.take()),
                '$' if self.looking_at("(") => text.push_str(&self.substitution()),
                _ => {}
            }
        }
    }

    /// Copies the lines up to and including the delimiter line onto `text`.
    fn skip_heredoc_body(&mut self, delimiter: &str, strip_tabs: bool, text: &mut String) {
        while self.at < self.chars.len() {
            let stop = (self.at..self.chars.len())
                .find(|&i| self.chars[i] == '\n')
                .map_or(self.chars.len(), |i| i + 1);
            let line: String = self.chars[self.at..stop].iter().collect();
            self.at = stop;
            text.push_str(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            let line = if strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if line == delimiter {
                return;
            }
        }
    }

    /// The body of `$'...'`, after the opening quote.
    fn ansi_quoted(&mut self) {
        let mut raw = String::new();
        while let Some(c) = self.take().filter(|&c| c != '\'') {
            raw.push(c);
            if c == '\\' {
                raw.extend(self.take());
            }
        }
        self.push_str(&unescape(&raw));
    }

    /// A backslash makes the next character part of the word, and joins a
    /// line break to the next line without starting a word of its own.
    fn escaped(&mut self, start: usize) {
        match self.take() {
            Some('\n') | None => {}
            Some(c) => {
                self.begin(start);
                self.push_str(c.encode_utf8(&mut [0; 4]));
            }
        }
    }

    fn skip_comment(&mut self) {
        while self.peek().is_some_and(|c| c != '\n') {
            self.at += 1;
        }
    }

    /// Reads the delimiter after `<<` and queues the body for the next line
    /// break. The delimiter is not an argument of the command.
    fn heredoc(&mut self) {
        self.at += 1;
        let strip_tabs = self.looking_at("-");
        if strip_tabs {
            self.at += 1;
        }
        while self.looking_at(" ") || self.looking_at("\t") {
            self.at += 1;
        }
        let (delimiter, quoted) = self.delimiter();
        if !delimiter.is_empty() {
            self.opened_here.push(self.opened.len());
            self.opened.push(Opened {
                delimiter,
                strip_tabs,
                quoted,
                owner: None,
            });
        }
    }

    fn delimiter(&mut self) -> (String, bool) {
        let mut delimiter = String::new();
        let mut quote = None;
        let mut quoted = false;
        while let Some(c) = self.peek() {
            match (quote, c) {
                (None, '\'' | '"') => {
                    quote = Some(c);
                    quoted = true;
                }
                (Some(open), c) if c == open => quote = None,
                (None, '\\') => quoted = true,
                (None, c) if c.is_whitespace() || ";&|()<>".contains(c) => break,
                (_, c) => delimiter.push(c),
            }
            self.at += 1;
        }
        (delimiter, quoted)
    }

    fn read_heredoc_bodies(&mut self) {
        for opened in std::mem::take(&mut self.opened) {
            let start = self.at;
            let mut end = start;
            let mut text = String::new();
            while self.at < self.chars.len() {
                let line_start = self.at;
                let stop = (self.at..self.chars.len())
                    .find(|&i| self.chars[i] == '\n')
                    .unwrap_or(self.chars.len());
                let line: String = self.chars[self.at..stop].iter().collect();
                self.at = (stop + 1).min(self.chars.len());
                let line = line.trim_end_matches('\r');
                let line = match opened.strip_tabs {
                    true => line.trim_start_matches('\t'),
                    false => line,
                };
                if line == opened.delimiter {
                    end = line_start;
                    break;
                }
                text.push_str(line);
                text.push('\n');
                end = self.at;
            }
            if let Some(owner) = opened.owner.and_then(|at| self.commands.get_mut(at)) {
                owner.heredocs.push(Heredoc {
                    delimiter: opened.delimiter,
                    quoted: opened.quoted,
                    strip_tabs: opened.strip_tabs,
                    text,
                    body: start..end,
                });
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn slice(text: &str, range: Range<usize>) -> String {
        text.chars().skip(range.start).take(range.len()).collect()
    }

    #[test]
    fn a_heredoc_body_belongs_to_the_command_that_opened_it() {
        let got = commands("git commit -F - <<'EOF'\nfeat: x\n\nRefs: 1\nEOF\ngit status");

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].words, ["git", "commit", "-F", "-"]);
        assert_eq!(got[0].heredocs[0].text, "feat: x\n\nRefs: 1\n");
        assert!(got[0].heredocs[0].quoted);
        assert_eq!(got[1].words, ["git", "status"]);
        assert!(got[1].heredocs.is_empty());
    }

    #[test]
    fn a_piped_heredoc_belongs_to_the_producer_not_the_consumer() {
        let got = commands("cat <<EOF | git commit -F -\nbody\nEOF");

        assert_eq!(got[0].words, ["cat"]);
        assert_eq!(got[0].heredocs[0].text, "body\n");
        assert!(!got[0].heredocs[0].quoted);
        assert_eq!(got[1].words, ["git", "commit", "-F", "-"]);
        assert!(got[1].heredocs.is_empty());
        assert!(got[1].piped);
        assert!(!got[0].piped);
    }

    #[test]
    fn a_heredoc_on_a_line_with_no_command_words_belongs_to_no_command() {
        let got = commands("<<EOF\nbody\nEOF\ngit status");

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].words, ["git", "status"]);
        assert!(got[0].heredocs.is_empty());
    }

    #[test]
    fn the_heredoc_body_range_covers_the_body_lines_only() {
        let script = "cat <<'EOF'\none\ntwo\nEOF\necho done";

        let got = commands(script);

        assert_eq!(slice(script, got[0].heredocs[0].body.clone()), "one\ntwo\n");
    }

    #[test]
    fn a_tab_stripped_heredoc_drops_leading_tabs_from_its_text() {
        let got = commands("cat <<-EOF\n\tone\n\tEOF\n");

        assert_eq!(got[0].heredocs[0].text, "one\n");
    }

    #[test]
    fn only_a_single_pipe_marks_the_next_command_as_piped() {
        let got = commands("a | b || c ; d |\n e");

        let piped: Vec<bool> = got.iter().map(|c| c.piped).collect();

        assert_eq!(piped, [false, true, false, false, true]);
    }

    #[test]
    fn each_word_keeps_its_span_in_the_source() {
        let script = "git commit -m \"a b\" 'c'  d";

        let got = commands(script);

        let words: Vec<String> = got[0]
            .spans
            .iter()
            .map(|s| slice(script, s.start..s.end))
            .collect();
        assert_eq!(words, ["git", "commit", "-m", "\"a b\"", "'c'", "d"]);
    }

    #[test]
    fn a_word_is_dynamic_only_when_the_shell_would_expand_it() {
        let got = commands(r#"x "$A" '$B' \$C $D "a\$b" $'e' "$(f)""#);

        let dynamic: Vec<bool> = got[0].spans.iter().map(|s| s.dynamic).collect();

        assert_eq!(
            dynamic,
            [false, true, false, false, true, false, false, true]
        );
    }

    #[test]
    fn a_line_continuation_joins_lines_without_starting_a_word() {
        let table = [
            (
                "playbook config set \\\nauto.budget.mainUsd 1000",
                vec!["playbook", "config", "set", "auto.budget.mainUsd", "1000"],
            ),
            ("playbook \\\nmode auto", vec!["playbook", "mode", "auto"]),
            ("playbook \\\n  mode auto", vec!["playbook", "mode", "auto"]),
            ("playbook mode \\\n", vec!["playbook", "mode"]),
            ("play\\\nbook mode", vec!["playbook", "mode"]),
            ("echo \\\n\"\"", vec!["echo", ""]),
        ];
        for (script, expected) in table {
            let got = commands(script);

            assert_eq!(got.len(), 1, "{script:?}");
            assert_eq!(got[0].words, expected, "{script:?}");
        }
    }

    #[test]
    fn a_heredoc_or_here_string_operator_ends_the_word_before_it() {
        let script = "git commit -F -<<EOF\nbody\nEOF\ngit commit -F -<<<text";

        let got = commands(script);

        assert_eq!(got[0].words, ["git", "commit", "-F", "-"]);
        let dash = &got[0].spans[3];
        assert_eq!(slice(script, dash.start..dash.end), "-");
        assert_eq!(got[1].words, ["git", "commit", "-F", "-", "<<<", "text"]);
        let dash = &got[1].spans[3];
        assert_eq!(slice(script, dash.start..dash.end), "-");
    }

    #[test]
    fn a_word_followed_by_a_backtick_is_dynamic() {
        let got = commands("echo foo`date`\necho bar `date`");

        let dynamic = |c: &Command| c.spans.iter().map(|s| s.dynamic).collect::<Vec<_>>();
        assert_eq!(got[0].words, ["echo", "foo"]);
        assert_eq!(dynamic(&got[0]), [false, true]);
        assert_eq!(got[2].words, ["echo", "bar"]);
        assert_eq!(dynamic(&got[2]), [false, false]);
    }

    #[test]
    fn a_dollar_double_quote_reads_like_a_plain_double_quote() {
        let got = commands(r#"echo $"a b" $"$X" c"#);

        let dynamic: Vec<bool> = got[0].spans.iter().map(|s| s.dynamic).collect();
        assert_eq!(got[0].words, ["echo", "a b", "$X", "c"]);
        assert_eq!(dynamic, [false, false, true, false]);
    }

    #[test]
    fn a_pipe_with_stderr_marks_the_next_command_as_piped() {
        let got = commands("a |& b && c");

        assert_eq!(got.len(), 3);
        assert_eq!(got[1].words, ["b"]);
        let piped: Vec<bool> = got.iter().map(|c| c.piped).collect();
        assert_eq!(piped, [false, true, false]);
    }

    #[test]
    fn a_substitution_in_double_quotes_is_read_whole_with_its_quotes_and_parens() {
        let script = "git commit -m \"$(cat <<'EOF'\nfix(hooks): say \"two words\" (twice)\n\nit's done\n\nCo-Authored-By: Claude\nEOF\n)\" --no-verify\ngit status";

        let got = commands(script);

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].words.len(), 5);
        assert!(got[0].words[3].starts_with("$(cat <<'EOF'\nfix(hooks): say \"two words\" (twice)"));
        assert!(got[0].words[3].ends_with("EOF\n)"));
        assert_eq!(got[0].words[4], "--no-verify");
        assert!(got[0].spans[3].dynamic);
        assert_eq!(
            slice(script, got[0].spans[3].start..got[0].spans[3].start + 1),
            "\""
        );
        assert_eq!(got[1].words, ["git", "status"]);
    }

    #[test]
    fn nested_quotes_and_substitutions_inside_a_substitution_stay_in_one_word() {
        let got = commands(r#"echo "a $(echo "b $(echo ")") c") d""#);

        assert_eq!(got.len(), 1);
        assert_eq!(got[0].words.len(), 2);
    }

    #[test]
    fn a_shift_inside_arithmetic_is_not_a_heredoc() {
        let table = [
            "echo $((1<<2))\ngit status",
            "echo \"$((1<<2))\"\ngit status",
            "echo $(( (1<<2) + 1 ))\ngit status",
        ];
        for script in table {
            let got = commands(script);

            assert_eq!(got.len(), 2, "{script:?}");
            assert_eq!(got[1].words, ["git", "status"], "{script:?}");
            assert!(got[0].heredocs.is_empty(), "{script:?}");
            assert!(got[0].spans[1].dynamic, "{script:?}");
        }
    }

    #[test]
    fn each_command_knows_how_many_parentheses_enclose_it() {
        let got = commands("a; (b; (c)); d $(e) | f");

        let depth: Vec<(&str, usize)> =
            got.iter().map(|c| (c.words[0].as_str(), c.depth)).collect();
        assert_eq!(
            depth,
            [("a", 0), ("b", 1), ("c", 2), ("d", 0), ("e", 1), ("f", 0)]
        );
    }

    /// One code per command: `s` inside a substitution, `j` joined to one,
    /// `o` opens one, `-` for a flag that is off.
    #[test]
    fn each_command_knows_how_it_meets_a_command_substitution() {
        let table = [
            ("a $(b) c; d", "--o s-- -j- ---"),
            ("a `b` c", "--o s-- -j-"),
            ("(a) && b", "--- ---"),
            ("x=$(a) b", "--o s-- -j-"),
            ("a $(b) | c", "--o s-- ---"),
            ("$(a; (b); c)", "--o s-- s-- s--"),
        ];
        for (script, expected) in table {
            let flag = |on: bool, code: char| if on { code } else { '-' };
            let got: Vec<String> = commands(script)
                .iter()
                .map(|c| {
                    [
                        flag(c.substitution, 's'),
                        flag(c.joined, 'j'),
                        flag(c.opens_substitution, 'o'),
                    ]
                    .iter()
                    .collect()
                })
                .collect();

            assert_eq!(got.join(" "), expected, "{script:?}");
        }
    }

    #[test]
    fn substitutions_are_read_with_their_quotes_and_heredocs() {
        let table: [(&str, bool, Vec<&str>); 9] = [
            (
                r#"`echo "\"" ; x a\ b`"#,
                true,
                vec![r#"echo "\"" ; x a\ b"#],
            ),
            ("$(( $(x) + 1 ))", true, vec!["x"]),
            ("a $(b (c)) d", true, vec!["b (c)"]),
            ("x `y` z", true, vec!["y"]),
            ("\"$(printf ')'; rm /a)\"", true, vec!["printf ')'; rm /a"]),
            ("'$(no)' \"it's $(yes)\"", true, vec!["yes"]),
            (
                "$(cat <<'E'\n1) `q`\nE\n)",
                true,
                vec!["cat <<'E'\n1) `q`\nE\n"],
            ),
            ("it's $(yes) $((1+2))", false, vec!["yes"]),
            ("$(open", true, vec!["open"]),
        ];
        for (text, quotes, expected) in table {
            assert_eq!(substitutions(text, quotes), expected, "{text:?}");
        }
    }

    #[test]
    fn options_and_the_shell_script_are_found_past_values_and_clusters() {
        let words: Vec<String> = ["-c", "3", "-x", "rm", "a"].map(String::from).to_vec();

        assert_eq!(skip_options(&words, 0, &["-c"]), 3);
        assert_eq!(skip_options(&words, 0, &[]), 1);
        assert_eq!(
            skip_options(&["--".to_string(), "-x".to_string()], 0, &[]),
            1
        );
        assert_eq!(skip_options(&["-n".to_string()], 0, &["-n"]), 1);
        assert_eq!(
            shell_script_at(&["-lc".to_string(), "x".to_string()]),
            Some(1)
        );
        assert_eq!(
            shell_script_at(&["--norc".to_string(), "-c".to_string()]),
            None
        );
    }

    #[test]
    fn a_word_before_a_closing_backtick_stays_dynamic() {
        let got = commands("echo `ls /a`");

        assert_eq!(got[1].words, ["ls", "/a"]);
        assert!(got[1].spans[1].dynamic);
    }

    #[test]
    fn sibling_parenthesis_groups_have_different_ids_and_nested_ones_differ_from_their_parent() {
        let got = commands("a; (b; c) && (d; (e)); f");

        let groups: Vec<(&str, usize)> =
            got.iter().map(|c| (c.words[0].as_str(), c.group)).collect();
        assert_eq!(
            groups,
            [("a", 0), ("b", 1), ("c", 1), ("d", 2), ("e", 3), ("f", 0)]
        );
    }

    #[test]
    fn a_command_line_ends_at_the_line_break_outside_quotes_substitutions_and_continuations() {
        let table = [
            ("a && \\\nb\nc", vec![Some(8), Some(8), None]),
            ("a \"x\ny\" ; b\nc", vec![Some(11), Some(11), None]),
            ("a \"$(cat <<'EOF'\nx\nEOF\n)\"\nb", vec![Some(25), None]),
            ("a &&\nb", vec![Some(4), None]),
            ("a # note\nb", vec![Some(8), None]),
        ];
        for (script, expected) in table {
            let got: Vec<Option<usize>> = commands(script).iter().map(|c| c.line_end).collect();

            assert_eq!(got, expected, "{script:?}");
        }
    }

    #[test]
    fn a_heredoc_remembers_whether_its_operator_stripped_tabs() {
        let got = commands("cat <<-A\n\tx\n\tA\ncat <<B\nx\nB\n");

        assert!(got[0].heredocs[0].strip_tabs);
        assert!(!got[1].heredocs[0].strip_tabs);
    }

    #[test]
    fn ansi_c_quoting_is_decoded() {
        let got = commands(r"git commit -m $'a\nb\tc \'q\' \x41'");

        assert_eq!(got[0].words[3], "a\nb\tc 'q' A");
    }

    #[test]
    fn a_double_quoted_backslash_keeps_unknown_escapes() {
        let got = commands(r#"printf "a\nb \"q\" \$x \\ c""#);

        assert_eq!(got[0].words[1], r#"a\nb "q" $x \ c"#);
    }

    #[test]
    fn unescape_resolves_the_printf_escapes() {
        assert_eq!(unescape(r"a\nb\tc\\d\x41\101é\q"), "a\nb\tc\\dAA\u{e9}\\q");
        assert_eq!(unescape("trailing\\"), "trailing\\");
    }

    #[test]
    fn quote_reads_back_as_the_same_word() {
        let text = "it's a \"test\"\nwith $x and `y`";

        let got = commands(&format!("echo {}", quote(text)));

        assert_eq!(got[0].words[1], text);
    }

    #[test]
    fn word_is_bare_only_when_every_character_is_safe() {
        let cases = [
            ("/repo/.worktrees/feat-x", "/repo/.worktrees/feat-x"),
            ("/R&D/a b", "'/R&D/a b'"),
            ("/x/a;id", "'/x/a;id'"),
            ("/x/it's", "'/x/it'\\''s'"),
            ("", "''"),
        ];

        for (text, want) in cases {
            assert_eq!(word(text), want, "{text}");
        }
    }

    #[test]
    fn apply_edits_replaces_character_ranges_and_skips_overlaps() {
        let mut edits = vec![
            (4..7, "XY".to_string()),
            (0..1, "z".to_string()),
            (5..6, "!".to_string()),
        ];

        assert_eq!(apply_edits("a \u{e9} bcd e", &mut edits), "z \u{e9} XY e");
    }

    #[test]
    fn apply_edits_skips_a_backwards_or_out_of_bounds_range() {
        let mut edits = vec![
            (Range { start: 3, end: 1 }, "x".to_string()),
            (2..99, "y".to_string()),
            (0..1, "z".to_string()),
            (50..60, "w".to_string()),
        ];

        assert_eq!(apply_edits("abcd", &mut edits), "zbcd");
    }

    #[test]
    fn simple_commands_keep_only_the_words() {
        let got = simple_commands("env A=1 git commit -m 'a b' && echo <<X\nignored\nX\n");

        assert_eq!(
            got,
            vec![
                vec!["env", "A=1", "git", "commit", "-m", "a b"],
                vec!["echo"],
            ]
        );
    }
}
