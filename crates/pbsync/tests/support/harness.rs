// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Two things for the command tests: a machine (a temp data folder with a real
//! `usage.db`) and a remote (a fake HTTP server in front of a SQLite database).

// Each test binary uses a different part of these helpers.
#![allow(dead_code)]

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use pbsync::cli::{run, Context};
use pbsync::settings::Settings;
use pbsync::transport::{Statement, Transport, Value};
use playbook_store::Connection;
use serde_json::{json, Value as Json};

use crate::local;
use crate::sqlite_fake::SqliteFake;
use crate::support::{FakeServer, Reply};

pub(crate) const NOW: i64 = 1_800_000_000;
pub(crate) const SENTINEL: &str = "sentinel-token-5d2e";
pub(crate) const TOKEN_ENV: &str = "PBSYNC_TOKEN";
static NEXT: AtomicU32 = AtomicU32::new(0);

pub(crate) struct Outcome {
    pub(crate) code: u8,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// A temp data folder holding `usage.db` and `sync.json`.
pub(crate) struct Machine {
    pub(crate) dir: PathBuf,
    pub(crate) id: String,
}

impl Machine {
    pub(crate) fn new(test: &str, id: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "{test}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let machine = Self {
            dir,
            id: id.to_string(),
        };
        machine
            .db()
            .execute("INSERT INTO machine (id, machine_id) VALUES (1, ?1)", [id])
            .unwrap();
        machine
    }

    /// The `usage.db`, created with the sync layout on first use.
    pub(crate) fn db(&self) -> Connection {
        let path = self.dir.join("usage.db");
        if path.exists() {
            Connection::open(path).unwrap()
        } else {
            local::open_at(&path)
        }
    }

    pub(crate) fn enable(&self, url: &str, accounts: &[&str]) {
        Settings {
            enabled: true,
            url: Some(url.to_string()),
            accounts: accounts.iter().map(|a| (*a).to_string()).collect(),
            ..Settings::default()
        }
        .save(&self.dir)
        .unwrap();
    }

    pub(crate) fn settings(&self) -> Settings {
        Settings::load(&self.dir).unwrap()
    }

    pub(crate) fn sync_json(&self) -> String {
        std::fs::read_to_string(self.dir.join("sync.json")).unwrap_or_default()
    }

    pub(crate) fn pb(&self, args: &[&str]) -> Outcome {
        self.pb_at(args, NOW, "", Some(SENTINEL))
    }

    pub(crate) fn pb_at(
        &self,
        args: &[&str],
        now: i64,
        stdin: &str,
        token: Option<&str>,
    ) -> Outcome {
        self.pb_on(args, now, stdin, token, false)
    }

    /// Like `pb`, with stderr reported as a terminal or not.
    pub(crate) fn pb_terminal(&self, args: &[&str], is_terminal: bool) -> Outcome {
        self.pb_on(args, NOW, "", Some(SENTINEL), is_terminal)
    }

    fn pb_on(
        &self,
        args: &[&str],
        now: i64,
        stdin: &str,
        token: Option<&str>,
        is_terminal: bool,
    ) -> Outcome {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        let env = |name: &str| {
            (name == TOKEN_ENV)
                .then(|| token.map(str::to_string))
                .flatten()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut input = Cursor::new(stdin.as_bytes().to_vec());
        let clock = move || now;
        let mut ctx = Context {
            data_dir: self.dir.clone(),
            env: &env,
            now: &clock,
            is_terminal: &move || is_terminal,
            input: &mut input,
            out: &mut out,
            err: &mut err,
        };
        let code = run(&args, &mut ctx);
        let outcome = Outcome {
            code,
            stdout: String::from_utf8(out).unwrap(),
            stderr: String::from_utf8(err).unwrap(),
        };
        for text in [&outcome.stdout, &outcome.stderr, &self.sync_json()] {
            assert!(!text.contains(SENTINEL), "the token leaked: {text}");
        }
        outcome
    }
}

/// A fake Turso server: real HTTP in front of a SQLite database.
pub(crate) struct Remote {
    pub(crate) url: String,
    server: FakeServer,
    fake: Arc<Mutex<SqliteFake>>,
    fail_on: Arc<Mutex<Option<String>>>,
}

impl Remote {
    pub(crate) fn start() -> Self {
        let fake = Arc::new(Mutex::new(SqliteFake::new()));
        let fail_on = Arc::new(Mutex::new(None::<String>));
        let (served, down) = (Arc::clone(&fake), Arc::clone(&fail_on));
        let server = FakeServer::start_with(move |_, received| {
            let broken = down.lock().unwrap();
            if broken
                .as_ref()
                .is_some_and(|text| received.body.contains(text))
            {
                return Reply::new(400, "{}");
            }
            let body: Json = serde_json::from_str(&received.body).unwrap();
            Reply::new(200, &answer(&served.lock().unwrap(), &body).to_string())
        });
        Self {
            url: server.url.clone(),
            server,
            fake,
            fail_on,
        }
    }

    /// From now on, a request whose body contains `text` is refused with HTTP 400.
    pub(crate) fn refuse_requests_containing(&self, text: &str) {
        *self.fail_on.lock().unwrap() = Some(text.to_string());
    }

    pub(crate) fn refuse_everything(&self) {
        self.refuse_requests_containing("");
    }

    pub(crate) fn requests(&self) -> usize {
        self.server.requests().len()
    }

    pub(crate) fn query(&self, sql: &str) -> Vec<Vec<Value>> {
        self.fake.lock().unwrap().query(sql)
    }

    pub(crate) fn count(&self, sql: &str) -> i64 {
        match self.query(sql)[0][0] {
            Value::Integer(n) => n,
            ref other => panic!("{other:?}"),
        }
    }
}

fn answer(fake: &SqliteFake, body: &Json) -> Json {
    let results: Vec<Json> = body["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|request| match request["type"].as_str().unwrap() {
            "execute" => execute(fake, &request["stmt"]),
            _ => json!({"type": "ok", "response": {"type": "close"}}),
        })
        .collect();
    json!({ "results": results })
}

fn execute(fake: &SqliteFake, stmt: &Json) -> Json {
    let args = stmt["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(decode)
        .collect();
    let statement = Statement {
        sql: stmt["sql"].as_str().unwrap().to_string(),
        args,
    };
    match fake.execute(&[statement]) {
        Ok(results) => {
            let result = &results[0];
            let cols: Vec<Json> = result.columns.iter().map(|c| json!({"name": c})).collect();
            let rows: Vec<Vec<Json>> = result
                .rows
                .iter()
                .map(|row| row.iter().map(encode).collect())
                .collect();
            json!({"type": "ok", "response": {"type": "execute", "result": {
                "cols": cols, "rows": rows, "affected_row_count": 0}}})
        }
        Err(error) => json!({"type": "error", "error": {"message": error.to_string()}}),
    }
}

fn decode(value: &Json) -> Value {
    let payload = &value["value"];
    match value["type"].as_str().unwrap() {
        "integer" => Value::Integer(payload.as_str().unwrap().parse().unwrap()),
        "float" => Value::Real(payload.as_f64().unwrap()),
        "text" => Value::Text(payload.as_str().unwrap().to_string()),
        _ => Value::Null,
    }
}

fn encode(value: &Value) -> Json {
    match value {
        Value::Null => json!({"type": "null"}),
        Value::Integer(n) => json!({"type": "integer", "value": n.to_string()}),
        Value::Real(x) => json!({"type": "float", "value": x}),
        Value::Text(s) => json!({"type": "text", "value": s}),
    }
}
