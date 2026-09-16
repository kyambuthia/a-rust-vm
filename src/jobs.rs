//! Artifact-oriented jobs for safe, deterministic guest data processing.
//!
//! This is deliberately a control-plane primitive, not a host command runner.
//! Jobs read named guest artifacts and write declared guest artifacts. The first
//! executor is a bounded CSV/TSV tabulator; future WASI and Linux executors can
//! use the same ownership, lifecycle, and artifact contract.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::runtime::{RuntimeError, VmInstance};

const MAX_TABLE_ROWS: usize = 50_000;
const MAX_TABLE_COLUMNS: usize = 256;
const PREVIEW_ROWS: usize = 10;
const PREVIEW_CELL_CHARS: usize = 160;
const MAX_JOB_SNAPSHOT_RECORDS: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub owner: String,
    pub executor: String,
    pub input_path: String,
    pub output_path: Option<String>,
    pub state: JobState,
    pub error: Option<String>,
}

/// In-memory job control plane for one server process.
#[derive(Debug, Default)]
pub struct JobStore {
    next_id: u64,
    jobs: Vec<JobRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStoreSnapshot {
    pub next_id: u64,
    pub jobs: Vec<JobRecord>,
}

impl JobStore {
    pub fn snapshot(&self) -> JobStoreSnapshot {
        JobStoreSnapshot {
            next_id: self.next_id,
            jobs: self.jobs.clone(),
        }
    }

    pub fn from_snapshot(mut snapshot: JobStoreSnapshot) -> Result<Self, String> {
        if snapshot.jobs.len() > MAX_JOB_SNAPSHOT_RECORDS {
            return Err(format!(
                "too many jobs in snapshot: maximum is {MAX_JOB_SNAPSHOT_RECORDS}"
            ));
        }
        let mut ids = BTreeSet::new();
        let mut highest_id = 0;
        for job in &mut snapshot.jobs {
            if !ids.insert(job.id.clone()) {
                return Err(format!("duplicate job id in snapshot: {}", job.id));
            }
            let Some(sequence) = job.id.strip_prefix("job-") else {
                return Err(format!("invalid job id in snapshot: {}", job.id));
            };
            let sequence = sequence
                .parse::<u64>()
                .map_err(|_| format!("invalid job id in snapshot: {}", job.id))?;
            if sequence == 0 {
                return Err(format!("invalid job id in snapshot: {}", job.id));
            }
            highest_id = highest_id.max(sequence);
            if matches!(job.state, JobState::Queued | JobState::Running) {
                job.state = JobState::Failed;
                job.error = Some("job interrupted by service restart".to_owned());
                job.output_path = None;
            }
        }
        if snapshot.next_id < highest_id {
            return Err("job counter is behind a stored job".to_owned());
        }
        Ok(Self {
            next_id: snapshot.next_id,
            jobs: snapshot.jobs,
        })
    }

    pub(crate) fn reconcile_output_paths(&mut self, vm: &VmInstance) {
        for job in &mut self.jobs {
            if job.state == JobState::Succeeded
                && job
                    .output_path
                    .as_deref()
                    .is_none_or(|path| vm.read_file(path).is_err())
            {
                job.state = JobState::Failed;
                job.output_path = None;
                job.error = Some("job output missing after service restart".to_owned());
            }
        }
    }

    pub fn start_tabulation(&mut self, owner: &str, input_path: &str) -> Result<JobRecord, String> {
        self.start(owner, "builtin.tabulate.v1", input_path)
    }

    pub fn try_start_tabulation(
        &mut self,
        owner: &str,
        input_path: &str,
        max_jobs: usize,
    ) -> Option<JobRecord> {
        self.try_start(owner, "builtin.tabulate.v1", input_path, max_jobs)
    }

    pub fn start_pdf_inspection(
        &mut self,
        owner: &str,
        input_path: &str,
    ) -> Result<JobRecord, String> {
        self.start(owner, "builtin.pdf_inspect.v1", input_path)
    }

    pub fn try_start_pdf_inspection(
        &mut self,
        owner: &str,
        input_path: &str,
        max_jobs: usize,
    ) -> Option<JobRecord> {
        self.try_start(owner, "builtin.pdf_inspect.v1", input_path, max_jobs)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn try_start_builtin_app(
        &mut self,
        owner: &str,
        app: &str,
        input_path: &str,
        max_jobs: usize,
    ) -> Option<JobRecord> {
        self.try_start(owner, &format!("builtin.{app}.v1"), input_path, max_jobs)
    }

    fn start(
        &mut self,
        owner: &str,
        executor: &str,
        input_path: &str,
    ) -> Result<JobRecord, String> {
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| "job id counter exhausted".to_owned())?;
        let job = JobRecord {
            id: format!("job-{}", self.next_id),
            owner: owner.to_owned(),
            executor: executor.to_owned(),
            input_path: input_path.to_owned(),
            output_path: None,
            state: JobState::Queued,
            error: None,
        };
        self.jobs.push(job.clone());
        Ok(job)
    }

    fn try_start(
        &mut self,
        owner: &str,
        executor: &str,
        input_path: &str,
        max_jobs: usize,
    ) -> Option<JobRecord> {
        if self.next_id == u64::MAX {
            return None;
        }
        let active_jobs = self
            .jobs
            .iter()
            .filter(|job| {
                job.owner == owner && matches!(job.state, JobState::Queued | JobState::Running)
            })
            .count();
        if active_jobs >= max_jobs {
            return None;
        }
        let owner_jobs = self.jobs.iter().filter(|job| job.owner == owner).count();
        if owner_jobs >= max_jobs {
            let terminal = self.jobs.iter().position(|job| {
                job.owner == owner && matches!(job.state, JobState::Succeeded | JobState::Failed)
            })?;
            self.jobs.remove(terminal);
        }
        self.start(owner, executor, input_path).ok()
    }

    pub fn mark_running(&mut self, id: &str) -> bool {
        if let Some(job) = self
            .jobs
            .iter_mut()
            .find(|job| job.id == id && job.state == JobState::Queued)
        {
            job.state = JobState::Running;
            return true;
        }
        false
    }

    pub fn finish(&mut self, id: &str, result: Result<&str, &str>) -> Option<JobRecord> {
        let job = self.jobs.iter_mut().find(|job| job.id == id)?;
        if job.state != JobState::Running {
            return None;
        }
        match result {
            Ok(output_path) => {
                job.state = JobState::Succeeded;
                job.output_path = Some(output_path.to_owned());
                job.error = None;
            }
            Err(error) => {
                job.state = JobState::Failed;
                job.output_path = None;
                job.error = Some(error.to_owned());
            }
        }
        Some(job.clone())
    }

    pub fn list(&self, owner: &str) -> Vec<JobRecord> {
        self.jobs
            .iter()
            .filter(|job| job.owner == owner)
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ColumnSummary {
    pub name: String,
    pub non_empty: usize,
    pub numeric: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableSummary {
    pub source_path: String,
    pub delimiter: String,
    pub rows: usize,
    pub columns: Vec<ColumnSummary>,
    pub preview: Vec<Vec<String>>,
    pub output_path: String,
}

/// Metadata collected from a PDF without invoking a PDF parser in the browser
/// host. Page count is an estimate based on page dictionaries, not a promise
/// about rendered pages; text extraction belongs in a dedicated parser sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PdfSummary {
    pub source_path: String,
    pub bytes: usize,
    pub version: String,
    pub page_objects_estimate: usize,
    pub output_path: String,
}

/// Tabulate a UTF-8 CSV or TSV input and persist a bounded JSON summary under
/// `/workspace/output`. Input is restricted to uploads so a browser request
/// cannot use this data path to inspect arbitrary guest files.
pub fn tabulate_uploaded_file(vm: &mut VmInstance, path: &str) -> Result<TableSummary, String> {
    let path = validate_upload_path(path)?;
    let input = vm.read_text(&path).map_err(runtime_error)?;
    let delimiter = detect_delimiter(&input);
    let records = parse_delimited(&input, delimiter)?;
    let (headers, rows) = records
        .split_first()
        .ok_or_else(|| "table is empty".to_owned())?;
    validate_headers(headers)?;
    if headers.len() > MAX_TABLE_COLUMNS {
        return Err(format!("table has more than {MAX_TABLE_COLUMNS} columns"));
    }
    if rows.len() > MAX_TABLE_ROWS {
        return Err(format!("table has more than {MAX_TABLE_ROWS} data rows"));
    }
    if let Some((row_number, row)) = rows
        .iter()
        .enumerate()
        .find(|(_, row)| row.len() != headers.len())
    {
        return Err(format!(
            "row {} has {} columns; expected {}",
            row_number + 2,
            row.len(),
            headers.len()
        ));
    }

    let columns = headers
        .iter()
        .enumerate()
        .map(|(index, name)| ColumnSummary {
            name: name.clone(),
            non_empty: rows
                .iter()
                .filter(|row| !row[index].trim().is_empty())
                .count(),
            numeric: rows
                .iter()
                .filter(|row| {
                    let cell = row[index].trim();
                    !cell.is_empty() && cell.parse::<f64>().is_ok()
                })
                .count(),
        })
        .collect();
    let output_path = format!("/workspace/output/{}.table.json", upload_filename(&path)?);
    let summary = TableSummary {
        source_path: path,
        delimiter: if delimiter == '\t' { "tsv" } else { "csv" }.to_owned(),
        rows: rows.len(),
        columns,
        preview: rows
            .iter()
            .take(PREVIEW_ROWS)
            .map(|row| row.iter().map(|cell| truncate_cell(cell)).collect())
            .collect(),
        output_path: output_path.clone(),
    };
    let encoded = serde_json::to_vec_pretty(&summary)
        .map_err(|error| format!("failed to encode table output: {error}"))?;
    vm.mkdir("/workspace/output", true).map_err(runtime_error)?;
    vm.write_file(&output_path, encoded)
        .map_err(runtime_error)?;
    Ok(summary)
}

/// Validate an uploaded PDF and persist a metadata artifact. This intentionally
/// does not parse page streams or execute embedded content in the web server.
pub fn inspect_uploaded_pdf(vm: &mut VmInstance, path: &str) -> Result<PdfSummary, String> {
    let path = validate_upload_path(path)?;
    let bytes = vm.read_file(&path).map_err(runtime_error)?;
    let version = pdf_version(&bytes)?;
    let output_path = format!("/workspace/output/{}.pdf.json", upload_filename(&path)?);
    let summary = PdfSummary {
        source_path: path,
        bytes: bytes.len(),
        version,
        page_objects_estimate: page_object_count(&bytes),
        output_path: output_path.clone(),
    };
    let encoded = serde_json::to_vec_pretty(&summary)
        .map_err(|error| format!("failed to encode PDF output: {error}"))?;
    vm.mkdir("/workspace/output", true).map_err(runtime_error)?;
    vm.write_file(&output_path, encoded)
        .map_err(runtime_error)?;
    Ok(summary)
}

fn validate_upload_path(path: &str) -> Result<String, String> {
    let filename = upload_filename(path)?;
    Ok(format!("/workspace/uploads/{filename}"))
}

fn pdf_version(bytes: &[u8]) -> Result<String, String> {
    let version = bytes
        .get(0..8)
        .filter(|header| header.starts_with(b"%PDF-"))
        .and_then(|header| std::str::from_utf8(&header[5..]).ok())
        .filter(|version| {
            let mut parts = version.split('.');
            matches!(
                (parts.next(), parts.next(), parts.next()),
                (Some(major), Some(minor), None)
                    if major.len() == 1
                        && minor.len() == 1
                        && major.as_bytes()[0].is_ascii_digit()
                        && minor.as_bytes()[0].is_ascii_digit()
            )
        })
        .ok_or_else(|| "upload is not a supported PDF header".to_owned())?;
    Ok(version.to_owned())
}

fn page_object_count(bytes: &[u8]) -> usize {
    bytes
        .windows(b"/Type".len())
        .enumerate()
        .filter_map(|(offset, value)| (value == b"/Type").then_some(offset + b"/Type".len()))
        .filter_map(|offset| {
            let remaining = bytes.get(offset..)?;
            let whitespace = remaining
                .iter()
                .take_while(|byte| byte.is_ascii_whitespace())
                .count();
            let value = remaining.get(whitespace..)?;
            value.starts_with(b"/Page").then_some(value.get(5).copied())
        })
        .filter(|next| !matches!(next, Some(b'A'..=b'Z' | b'a'..=b'z')))
        .count()
}

fn upload_filename(path: &str) -> Result<&str, String> {
    let filename = path
        .strip_prefix("/workspace/uploads/")
        .ok_or_else(|| "tabulation input must be an uploaded file".to_owned())?;
    if filename.is_empty() || filename.contains('/') || filename.contains('\\') {
        return Err("tabulation input must name one uploaded file".to_owned());
    }
    Ok(filename)
}

fn detect_delimiter(input: &str) -> char {
    let first_line = input
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    if first_line.matches('\t').count() > first_line.matches(',').count() {
        '\t'
    } else {
        ','
    }
}

fn validate_headers(headers: &[String]) -> Result<(), String> {
    if headers.is_empty() || headers.iter().any(|header| header.trim().is_empty()) {
        return Err("table must have non-empty column names".to_owned());
    }
    let unique = headers.iter().collect::<BTreeSet<_>>();
    if unique.len() != headers.len() {
        return Err("table column names must be unique".to_owned());
    }
    Ok(())
}

fn truncate_cell(cell: &str) -> String {
    let mut characters = cell.chars();
    let preview = characters
        .by_ref()
        .take(PREVIEW_CELL_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn runtime_error(error: RuntimeError) -> String {
    error.to_string()
}

fn parse_delimited(input: &str, delimiter: char) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut after_quote = false;
    let mut characters = input.chars().peekable();

    while let Some(character) = characters.next() {
        if quoted {
            if character == '"' {
                if characters.peek() == Some(&'"') {
                    characters.next();
                    cell.push('"');
                } else {
                    quoted = false;
                    after_quote = true;
                }
            } else {
                cell.push(character);
            }
            continue;
        }
        if after_quote {
            match character {
                value if value == delimiter => {
                    row.push(std::mem::take(&mut cell));
                    after_quote = false;
                }
                '\n' => {
                    row.push(std::mem::take(&mut cell));
                    rows.push(std::mem::take(&mut row));
                    after_quote = false;
                }
                '\r' if characters.peek() == Some(&'\n') => {}
                value if value.is_whitespace() => {}
                _ => return Err("unexpected text after a quoted table cell".to_owned()),
            }
            continue;
        }
        match character {
            '"' if cell.is_empty() => quoted = true,
            value if value == delimiter => row.push(std::mem::take(&mut cell)),
            '\n' => {
                row.push(std::mem::take(&mut cell));
                rows.push(std::mem::take(&mut row));
            }
            '\r' if characters.peek() == Some(&'\n') => {}
            value => cell.push(value),
        }
    }
    if quoted {
        return Err("table has an unterminated quoted cell".to_owned());
    }
    if after_quote || !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows.retain(|row| !row.iter().all(|cell| cell.trim().is_empty()));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::{
        JobState, JobStore, JobStoreSnapshot, inspect_uploaded_pdf, tabulate_uploaded_file,
    };
    use crate::runtime::VmInstance;

    #[test]
    fn tabulates_csv_into_a_guest_output_artifact() {
        let mut vm = VmInstance::new("tabulation");
        vm.mkdir("/workspace/uploads", true).unwrap();
        vm.write_file(
            "/workspace/uploads/sales.csv",
            "region,amount\nEast,12\nWest,18\n",
        )
        .unwrap();

        let summary = tabulate_uploaded_file(&mut vm, "/workspace/uploads/sales.csv").unwrap();

        assert_eq!(summary.rows, 2);
        assert_eq!(summary.columns[1].numeric, 2);
        assert_eq!(
            summary.output_path,
            "/workspace/output/sales.csv.table.json"
        );
        assert!(vm.read_text(&summary.output_path).unwrap().contains("East"));
    }

    #[test]
    fn rejects_non_upload_inputs_and_irregular_rows() {
        let mut vm = VmInstance::new("tabulation");
        assert!(tabulate_uploaded_file(&mut vm, "/tmp/nope.csv").is_err());
        vm.mkdir("/workspace/uploads", true).unwrap();
        vm.write_file("/workspace/uploads/bad.csv", "a,b\n1\n")
            .unwrap();
        assert!(tabulate_uploaded_file(&mut vm, "/workspace/uploads/bad.csv").is_err());
    }

    #[test]
    fn jobs_track_success_and_failure() {
        let mut store = JobStore::default();
        let job = store
            .start_tabulation("local", "/workspace/uploads/a.csv")
            .unwrap();
        store.mark_running(&job.id);
        let completed = store
            .finish(&job.id, Ok("/workspace/output/a.json"))
            .unwrap();
        assert_eq!(completed.state, JobState::Succeeded);
        assert_eq!(store.list("local").len(), 1);
    }

    #[test]
    fn try_start_enforces_the_owner_limit_before_inserting() {
        let mut store = JobStore::default();

        assert!(
            store
                .try_start_tabulation("local", "/workspace/uploads/a.csv", 1)
                .is_some()
        );
        assert!(
            store
                .try_start_pdf_inspection("local", "/workspace/uploads/b.pdf", 1)
                .is_none()
        );
        assert_eq!(store.list("local").len(), 1);
    }

    #[test]
    fn restoring_jobs_fails_in_flight_work_closed() {
        let mut store = JobStore::default();
        let job = store
            .try_start_tabulation("alice", "/workspace/uploads/a.csv", 4)
            .unwrap();
        store.mark_running(&job.id);

        let restored = JobStore::from_snapshot(store.snapshot()).unwrap();
        let jobs = restored.list("alice");
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];

        assert_eq!(job.state, JobState::Failed);
        assert_eq!(
            job.error.as_deref(),
            Some("job interrupted by service restart")
        );
        assert!(
            JobStore::from_snapshot(JobStoreSnapshot {
                next_id: 1,
                jobs: vec![job.clone(), job.clone()],
            })
            .is_err()
        );
    }

    #[test]
    fn completed_jobs_release_capacity_without_unbounded_history() {
        let mut store = JobStore::default();
        let first = store
            .try_start_tabulation("alice", "/workspace/uploads/a.csv", 2)
            .unwrap();
        assert!(store.mark_running(&first.id));
        assert!(
            store
                .finish(&first.id, Ok("/workspace/output/a.json"))
                .is_some()
        );

        let second = store
            .try_start_pdf_inspection("alice", "/workspace/uploads/b.pdf", 2)
            .unwrap();
        assert!(store.mark_running(&second.id));
        assert!(store.finish(&second.id, Err("bad pdf")).is_some());

        let third = store
            .try_start_tabulation("alice", "/workspace/uploads/c.csv", 2)
            .unwrap();
        assert_eq!(store.list("alice").len(), 2);
        assert!(store.list("alice").iter().all(|job| job.id != first.id));
        assert!(store.list("alice").iter().any(|job| job.id == third.id));
    }

    #[test]
    fn exhausted_job_ids_fail_without_mutating_the_store() {
        let mut store = JobStore::from_snapshot(JobStoreSnapshot {
            next_id: u64::MAX,
            jobs: Vec::new(),
        })
        .unwrap();

        assert!(
            store
                .start_tabulation("alice", "/workspace/uploads/a.csv")
                .is_err()
        );
        assert!(
            store
                .try_start_tabulation("alice", "/workspace/uploads/a.csv", 1)
                .is_none()
        );
        assert!(store.list("alice").is_empty());
    }

    #[test]
    fn restoring_jobs_rejects_unbounded_history() {
        let jobs = (1..=super::MAX_JOB_SNAPSHOT_RECORDS as u64 + 1)
            .map(|id| crate::jobs::JobRecord {
                id: format!("job-{id}"),
                owner: "alice".to_owned(),
                executor: "builtin.tabulate.v1".to_owned(),
                input_path: "/workspace/uploads/input.csv".to_owned(),
                output_path: None,
                state: JobState::Failed,
                error: Some("test".to_owned()),
            })
            .collect();
        assert!(
            JobStore::from_snapshot(JobStoreSnapshot {
                next_id: super::MAX_JOB_SNAPSHOT_RECORDS as u64 + 1,
                jobs,
            })
            .is_err()
        );
    }

    #[test]
    fn restoring_jobs_fails_when_a_success_output_is_missing() {
        let mut store = JobStore::default();
        let job = store
            .try_start_tabulation("alice", "/workspace/uploads/input.csv", 1)
            .unwrap();
        assert!(store.mark_running(&job.id));
        assert!(
            store
                .finish(&job.id, Ok("/workspace/output/missing.json"))
                .is_some()
        );

        let mut restored = JobStore::from_snapshot(store.snapshot()).unwrap();
        restored.reconcile_output_paths(&VmInstance::new("session"));

        let job = &restored.list("alice")[0];
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(job.output_path, None);
        assert_eq!(
            job.error.as_deref(),
            Some("job output missing after service restart")
        );
    }

    #[test]
    fn inspects_a_pdf_without_parsing_its_content_streams() {
        let mut vm = VmInstance::new("pdf");
        vm.mkdir("/workspace/uploads", true).unwrap();
        vm.write_file(
            "/workspace/uploads/guide.pdf",
            b"%PDF-1.7\n1 0 obj << /Type /Page >> endobj\n2 0 obj << /Type /Pages >> endobj",
        )
        .unwrap();

        let summary = inspect_uploaded_pdf(&mut vm, "/workspace/uploads/guide.pdf").unwrap();

        assert_eq!(summary.version, "1.7");
        assert_eq!(summary.page_objects_estimate, 1);
        assert!(
            vm.read_text(&summary.output_path)
                .unwrap()
                .contains("guide.pdf")
        );
    }

    #[test]
    fn rejects_a_non_pdf_upload() {
        let mut vm = VmInstance::new("pdf");
        vm.mkdir("/workspace/uploads", true).unwrap();
        vm.write_file("/workspace/uploads/notes.txt", "not a PDF")
            .unwrap();
        assert!(inspect_uploaded_pdf(&mut vm, "/workspace/uploads/notes.txt").is_err());
    }
}
