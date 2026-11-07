//! Internal JSONL records. Join attempts by context and attempt number, and cache
//! operations by operation ID. A start without a result is unknown, not success.
//! Sequence gaps and dropped-record counts identify write failures. Ignore an
//! incomplete JSON line left by a crash. Retention uses UTC calendar dates.

use std::{
    cell::RefCell,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::Instant,
};

use anyhow::{Context as _, Result, ensure};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, File, MetadataExt, OpenOptions, OpenOptionsExt};
use chrono::{Days, NaiveDate, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::model::now;

#[derive(Clone, Copy, Serialize)]
pub(crate) struct Context {
    pub chat_id: Uuid,
    pub run_id: Option<Uuid>,
    pub round: Option<usize>,
}

pub(crate) struct Metrics {
    session_id: Uuid,
    state: RefCell<State>,
}

struct State {
    dir: Option<Dir>,
    file: Option<File>,
    day: Option<NaiveDate>,
    sequence: u64,
    dropped_records: u64,
    warned: bool,
}

#[derive(Serialize)]
struct Record<'a> {
    schema_version: u32,
    session_id: Uuid,
    sequence: u64,
    dropped_records: u64,
    timestamp_ms: u64,
    event: &'a str,
    context: Option<Context>,
    details: Value,
}

impl Metrics {
    pub(crate) fn open(project_dir: &Dir) -> Self {
        let mut state = State {
            dir: None,
            file: None,
            day: None,
            sequence: 0,
            dropped_records: 0,
            warned: false,
        };
        match open_directory(project_dir) {
            Ok(dir) => {
                state.dir = Some(dir);
                if let Err(error) = state.open_day(Utc::now().date_naive()) {
                    state.warn(&error);
                }
            }
            Err(error) => state.warn(&error),
        }
        let metrics = Self {
            session_id: Uuid::new_v4(),
            state: RefCell::new(state),
        };
        metrics.record(
            "session_started",
            None,
            json!({
                "app_version": env!("CARGO_PKG_VERSION"),
                "cache_policy_version": 1,
            }),
        );
        metrics
    }

    pub(crate) fn record(&self, event: &str, context: Option<Context>, details: Value) {
        let mut state = self.state.borrow_mut();
        state.sequence += 1;
        let record = Record {
            schema_version: 1,
            session_id: self.session_id,
            sequence: state.sequence,
            dropped_records: state.dropped_records,
            timestamp_ms: now(),
            event,
            context,
            details,
        };
        if let Err(error) = state.append(&record) {
            state.file = None;
            state.dropped_records += 1;
            state.warn(&error);
        }
    }

    pub(crate) fn operation(
        &self,
        context: Option<Context>,
        operation: &str,
        reason: &str,
        details: Value,
    ) -> Operation<'_> {
        let id = Uuid::new_v4();
        self.record(
            "cache_operation_started",
            context,
            json!({
                "operation_id": id,
                "operation": operation,
                "reason": reason,
                "parameters": details,
            }),
        );
        Operation {
            metrics: self,
            context,
            id,
            started: Instant::now(),
        }
    }
}

impl Drop for Metrics {
    fn drop(&mut self) {
        self.record("session_finished", None, json!({}));
    }
}

pub(crate) struct Operation<'a> {
    metrics: &'a Metrics,
    context: Option<Context>,
    id: Uuid,
    started: Instant,
}

impl Operation<'_> {
    pub(crate) fn finish(self, outcome: &str, http_status: Option<u16>, details: Value) {
        let duration_ms = self.started.elapsed().as_secs_f64() * 1_000.;
        self.metrics.record(
            "cache_operation_finished",
            self.context,
            json!({
                "operation_id": self.id,
                "outcome": outcome,
                "http_status": http_status,
                "duration_ms": duration_ms,
                "result": details,
            }),
        );
    }
}

impl State {
    fn append(&mut self, record: &Record<'_>) -> Result<()> {
        let day = Utc::now().date_naive();
        if self.day != Some(day) || self.file.is_none() {
            self.open_day(day)?;
        }
        let mut bytes = serde_json::to_vec(record)?;
        bytes.push(b'\n');
        let file = self.file.as_mut().expect("The daily metrics file is open");
        file.write_all(&bytes)?;
        Ok(())
    }

    fn open_day(&mut self, day: NaiveDate) -> Result<()> {
        let dir = self
            .dir
            .as_ref()
            .context("Metrics storage is unavailable")?;
        let prune_error = prune(dir, day).err();
        let mut options = OpenOptions::new();
        options
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NONBLOCK)
            .follow(FollowSymlinks::No);
        let mut file = dir.open_with(format!("{day}.jsonl"), &options)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.nlink() == 1,
            "Invalid metrics file"
        );
        if metadata.len() > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        self.file = Some(file);
        self.day = Some(day);
        if let Some(error) = prune_error {
            self.warn(&error);
        }
        Ok(())
    }

    fn warn(&mut self, error: &anyhow::Error) {
        if !self.warned {
            eprintln!("space: Cannot maintain cache metrics: {error:#}");
            self.warned = true;
        }
    }
}

fn open_directory(project_dir: &Dir) -> Result<Dir> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match project_dir.create_dir_with("cache-metrics", &builder) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(project_dir.open_dir_nofollow("cache-metrics")?)
}

fn prune(dir: &Dir, today: NaiveDate) -> Result<()> {
    let oldest = today
        .checked_sub_days(Days::new(89))
        .unwrap_or(NaiveDate::MIN);
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(date) = name.strip_suffix(".jsonl") else {
            continue;
        };
        let Ok(day) = NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
            continue;
        };
        if day >= oldest || name != format!("{day}.jsonl") {
            continue;
        }
        let metadata = dir.symlink_metadata(Path::new(name))?;
        if metadata.is_file() && metadata.nlink() == 1 {
            dir.remove_file(name)?;
        }
    }
    Ok(())
}
