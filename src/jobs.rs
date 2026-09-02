//! Artifact-oriented jobs for safe, deterministic guest data processing.
//!
//! This is deliberately a control-plane primitive, not a host command runner.
//! Jobs read named guest artifacts and write declared guest artifacts. The first
//! executor is a bounded CSV/TSV tabulator; future WASI and Linux executors can
//! use the same ownership, lifecycle, and artifact contract.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::runtime::{RuntimeError, VmInstance};

const MAX_TABLE_ROWS: usize = 50_000;
const MAX_TABLE_COLUMNS: usize = 256;
const PREVIEW_ROWS: usize = 10;
const PREVIEW_CELL_CHARS: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

impl JobStore {
    pub fn start_tabulation(&mut self, owner: &str, input_path: &str) -> JobRecord {
        self.next_id += 1;
        let job = JobRecord {
            id: format!("job-{}", self.next_id),
            owner: owner.to_owned(),
            executor: "builtin.tabulate.v1".to_owned(),
            input_path: input_path.to_owned(),
            output_path: None,
            state: JobState::Queued,
            error: None,
        };
        self.jobs.push(job.clone());
        job
    }

    pub fn mark_running(&mut self, id: &str) {
        if let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) {
            job.state = JobState::Running;
        }
    }

    pub fn finish(&mut self, id: &str, result: Result<&str, &str>) -> Option<JobRecord> {
        let job = self.jobs.iter_mut().find(|job| job.id == id)?;
        match result {
            Ok(output_path) => {
                job.state = JobState::Succeeded;
                job.output_path = Some(output_path.to_owned());
            }
            Err(error) => {
                job.state = JobState::Failed;
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

fn validate_upload_path(path: &str) -> Result<String, String> {
    let filename = upload_filename(path)?;
    Ok(format!("/workspace/uploads/{filename}"))
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
    use super::{JobState, JobStore, tabulate_uploaded_file};
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
        let job = store.start_tabulation("local", "/workspace/uploads/a.csv");
        store.mark_running(&job.id);
        let completed = store
            .finish(&job.id, Ok("/workspace/output/a.json"))
            .unwrap();
        assert_eq!(completed.state, JobState::Succeeded);
        assert_eq!(store.list("local").len(), 1);
    }
}
