// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Keeps the view fresh without blocking the screen. A background thread owns
//! the database connection: it ingests new transcript lines and re-reads the
//! store on a timer, and at once when the range or filter changes, with the
//! same ingest `usage --summary` runs. A first read of a large history goes
//! in small batches and the screen shows the partial store as it grows.

use super::account::AccountSel;
use super::data::{self, Data};
use super::machine::MachineSel;
use crate::query::Range;
use crate::run::{ingest_into, Paths};
use crate::util::time::now_secs;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the store is re-read when nothing else asks.
pub const REFRESH_SECONDS: u64 = 5;

pub enum Request {
    /// Show this range and filter now, then keep refreshing them.
    Query {
        range: Range,
        filter: String,
        account: AccountSel,
        machine: MachineSel,
    },
    /// Delete the usage history, then read every transcript again.
    Reset,
}

pub enum Update {
    Data(Box<Data>),
    Error(String),
    /// Files read so far and files to read, while a first read is running;
    /// `None` once it is done.
    Indexing(Option<(usize, usize)>),
}

pub struct Worker {
    requests: Sender<Request>,
    pub updates: Receiver<Update>,
    handle: Option<JoinHandle<()>>,
}

/// How often a partial snapshot is sent while a first read runs.
const PARTIAL_EVERY: Duration = Duration::from_secs(1);

/// The range, filter, account and machine a snapshot is read for.
type Query = (Range, String, AccountSel, MachineSel);

/// The name of this machine, `None` when its row cannot be read; the hostname when no name is set.
fn this_machine_name(conn: &playbook_store::Connection) -> Option<String> {
    let machine = crate::machine::stored(conn).ok().flatten()?;
    Some(
        machine
            .name_or(&crate::machine::current_hostname())
            .to_string(),
    )
}

/// Reads the store into a snapshot.
fn snapshot(conn: &playbook_store::Connection, paths: &Paths, query: &Query) -> Update {
    match data::load(conn, query.0, &query.1, &query.2, &query.3, now_secs()) {
        Ok(mut d) => {
            if let Some(name) = this_machine_name(conn) {
                d.machines.name_local(&name);
            }
            // A limits failure never hides the rest of the view.
            if let Ok(l) = data::load_limits(conn, paths, query.0, d.accounts.key.as_deref(), d.now)
            {
                d.limits = l;
            }
            Update::Data(Box::new(d))
        }
        Err(e) => Update::Error(e),
    }
}

/// Applies a request: a new query replaces the old one and a reset is noted
/// for the loop to carry out before its next read.
fn apply(request: Request, query: &mut Query, reset: &mut bool) {
    match request {
        Request::Query {
            range,
            filter,
            account,
            machine,
        } => *query = (range, filter, account, machine),
        Request::Reset => *reset = true,
    }
}

impl Worker {
    pub fn spawn(
        paths: Paths,
        range: Range,
        filter: String,
        account: AccountSel,
        machine: MachineSel,
        every: Duration,
    ) -> Worker {
        let (requests, request_rx) = mpsc::channel::<Request>();
        let (update_tx, updates) = mpsc::channel::<Update>();
        let handle = std::thread::spawn(move || {
            let mut query = (range, filter, account, machine);
            let mut first = true;
            let mut reset = false;
            loop {
                let conn = match crate::db::open_db(&paths.db) {
                    Ok(c) => c,
                    Err(e) => {
                        if update_tx.send(Update::Error(e)).is_err() {
                            return;
                        }
                        match request_rx.recv_timeout(every) {
                            Ok(request) => apply(request, &mut query, &mut reset),
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                        continue;
                    }
                };
                if reset {
                    reset = false;
                    let update = match crate::db::reset_usage(&conn) {
                        Ok(()) => snapshot(&conn, &paths, &query),
                        Err(e) => Update::Error(e),
                    };
                    if update_tx.send(update).is_err() {
                        return;
                    }
                }
                // Show what the store holds before any ingest, so the screen
                // never waits on a first read of a large history.
                if first {
                    first = false;
                    if update_tx.send(snapshot(&conn, &paths, &query)).is_err() {
                        return;
                    }
                }
                let mut gone = false;
                let mut last_sent = std::time::Instant::now();
                let ingested = ingest_into(&paths, &conn, &mut |done, total| {
                    if total == 0 || done == total {
                        return true;
                    }
                    while let Ok(request) = request_rx.try_recv() {
                        apply(request, &mut query, &mut reset);
                        last_sent = std::time::Instant::now() - PARTIAL_EVERY;
                    }
                    if update_tx
                        .send(Update::Indexing(Some((done, total))))
                        .is_err()
                    {
                        gone = true;
                    }
                    if last_sent.elapsed() >= PARTIAL_EVERY {
                        last_sent = std::time::Instant::now();
                        if update_tx.send(snapshot(&conn, &paths, &query)).is_err() {
                            gone = true;
                        }
                    }
                    // A reset stops this read; the loop deletes, then starts over.
                    !gone && !reset
                });
                if gone || update_tx.send(Update::Indexing(None)).is_err() {
                    return;
                }
                let update = match ingested {
                    Ok(_) => snapshot(&conn, &paths, &query),
                    Err(e) => Update::Error(e),
                };
                if update_tx.send(update).is_err() {
                    return;
                }
                if reset {
                    continue;
                }
                match request_rx.recv_timeout(every) {
                    Ok(request) => apply(request, &mut query, &mut reset),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
        Worker {
            requests,
            updates,
            handle: Some(handle),
        }
    }

    /// Deletes the usage history and reads the transcripts again.
    pub fn reset(&self) {
        let _ = self.requests.send(Request::Reset);
    }

    pub fn ask(&self, range: Range, filter: &str, account: &AccountSel, machine: &MachineSel) {
        let _ = self.requests.send(Request::Query {
            range,
            filter: filter.to_string(),
            account: account.clone(),
            machine: machine.clone(),
        });
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Closing the request channel ends the loop after its current pass; the
        // handle is dropped without a join so quitting never waits on an ingest.
        drop(self.handle.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{scratch_paths, within};

    const BOUND: Duration = Duration::from_secs(20);

    fn count(conn: &playbook_store::Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn a_reset_deletes_the_stored_history_and_reads_the_transcripts_again() {
        // Arrange: a store read once from the transcripts, plus a row no transcript holds.
        let mut paths = scratch_paths("worker-reset");
        paths.claude_projects =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/usage/usage");
        let worker = Worker::spawn(
            paths.clone(),
            Range::All,
            String::new(),
            AccountSel::default(),
            MachineSel::default(),
            Duration::from_secs(1),
        );
        let conn = crate::db::open_db(&paths.db).unwrap();
        let rows = "SELECT COUNT(*) FROM usage_events";
        assert!(within(BOUND, || count(&conn, rows) > 0));
        let real = count(&conn, rows);
        assert!(within(BOUND, || {
            count(
                &conn,
                "SELECT COUNT(*) FROM usage_watermarks WHERE source = 'claude-code'",
            ) == 1
        }));
        conn.execute(
            "INSERT INTO usage_events (event_id, timestamp, session_id, account, model, effort,
                repo, branch, input_tokens, output_tokens, cache_creation_tokens,
                cache_read_tokens, cost_usd)
             VALUES ('stale', 1, 's', 'a', 'm', '', 'r', 'b', 1, 1, 0, 0, 1.0)",
            [],
        )
        .unwrap();

        // Act
        worker.reset();

        // Assert: the stale row is gone and the transcripts fill the store again.
        let stale = "SELECT COUNT(*) FROM usage_events WHERE event_id = 'stale'";
        assert!(within(BOUND, || count(&conn, stale) == 0));
        assert!(within(BOUND, || count(&conn, rows) == real));
    }
}
