//! Built-in deterministic application runtimes for one guest VM.
//!
//! These apps deliberately operate on fixed guest paths. They do not accept
//! executable paths, host paths, command lines, or ambient capabilities.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::runtime::{RuntimeError, VmInstance};

pub const DOCS_ROOT: &str = "/workspace/apps/docs";
pub const DOCS_DOCUMENT_PATH: &str = "/workspace/apps/docs/document.txt";
pub const DOCS_METADATA_PATH: &str = "/workspace/apps/docs/document.json";
pub const SHEETS_ROOT: &str = "/workspace/apps/sheets";
pub const SHEETS_INPUT_PATH: &str = "/workspace/apps/sheets/sheet.txt";
pub const SHEETS_SUMMARY_PATH: &str = "/workspace/apps/sheets/summary.json";

const MAX_DOCUMENT_BYTES: usize = 512 * 1024;
const MAX_SHEET_BYTES: usize = 512 * 1024;
const MAX_SHEET_ROWS: usize = 50_000;
const MAX_SHEET_COLUMNS: usize = 256;
const MAX_SHEET_CELLS: usize = 100_000;
const PREVIEW_ROWS: usize = 12;
const PREVIEW_CELL_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppId {
    Docs,
    Sheets,
}

impl AppId {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Docs => "Docs",
            Self::Sheets => "Sheets",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppDescriptor {
    pub id: AppId,
    pub label: &'static str,
    pub root: &'static str,
    pub operations: &'static [&'static str],
}

pub fn app_descriptors() -> [AppDescriptor; 2] {
    [
        AppDescriptor {
            id: AppId::Docs,
            label: AppId::Docs.label(),
            root: DOCS_ROOT,
            operations: &["open", "open_upload", "read", "replace", "append"],
        },
        AppDescriptor {
            id: AppId::Sheets,
            label: AppId::Sheets.label(),
            root: SHEETS_ROOT,
            operations: &["open", "open_upload", "import"],
        },
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Document {
    pub path: &'static str,
    pub bytes: usize,
    pub lines: usize,
    pub text: String,
    pub format: String,
    pub source_name: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DocumentMetadata {
    format: String,
    source_name: Option<String>,
    warnings: Vec<String>,
}

pub fn open_docs(vm: &mut VmInstance) -> Result<Document, String> {
    vm.mkdir(DOCS_ROOT, true).map_err(runtime_error)?;
    if vm.read_file(DOCS_DOCUMENT_PATH).is_err() {
        vm.write_file(DOCS_DOCUMENT_PATH, b"")
            .map_err(runtime_error)?;
    }
    read_document(vm)
}

pub fn read_document(vm: &VmInstance) -> Result<Document, String> {
    let text = vm.read_text(DOCS_DOCUMENT_PATH).map_err(runtime_error)?;
    if text.len() > MAX_DOCUMENT_BYTES {
        return Err(format!("document exceeds {MAX_DOCUMENT_BYTES} bytes"));
    }
    let metadata = vm
        .read_text(DOCS_METADATA_PATH)
        .ok()
        .and_then(|encoded| serde_json::from_str::<DocumentMetadata>(&encoded).ok())
        .unwrap_or_else(plain_document_metadata);
    Ok(Document {
        path: DOCS_DOCUMENT_PATH,
        bytes: text.len(),
        lines: line_count(&text),
        text,
        format: metadata.format,
        source_name: metadata.source_name,
        warnings: metadata.warnings,
    })
}

pub fn replace_document(vm: &mut VmInstance, text: &str) -> Result<Document, String> {
    let metadata = plain_document_metadata();
    replace_document_with_metadata(
        vm,
        text,
        (metadata.format, metadata.source_name, metadata.warnings),
    )
}

/// Persist normalized text extracted by a format reader. The reader has already
/// consumed copied guest bytes outside the VM lock; this function only writes
/// bounded guest artifacts.
pub fn replace_document_with_metadata(
    vm: &mut VmInstance,
    text: &str,
    metadata: (String, Option<String>, Vec<String>),
) -> Result<Document, String> {
    validate_document(text)?;
    vm.mkdir(DOCS_ROOT, true).map_err(runtime_error)?;
    vm.write_file(DOCS_DOCUMENT_PATH, text)
        .map_err(runtime_error)?;
    let metadata = DocumentMetadata {
        format: metadata.0,
        source_name: metadata.1,
        warnings: metadata.2,
    };
    let encoded = serde_json::to_vec_pretty(&metadata)
        .map_err(|error| format!("failed to encode document metadata: {error}"))?;
    vm.write_file(DOCS_METADATA_PATH, encoded)
        .map_err(runtime_error)?;
    read_document(vm)
}

pub fn append_document(vm: &mut VmInstance, text: &str) -> Result<Document, String> {
    validate_document(text)?;
    let current = open_docs(vm)?.text;
    let combined = format!("{current}{text}");
    validate_document(&combined)?;
    vm.write_file(DOCS_DOCUMENT_PATH, combined)
        .map_err(runtime_error)?;
    read_document(vm)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetFormat {
    Csv,
    Tsv,
}

impl SheetFormat {
    const fn delimiter(self) -> char {
        match self {
            Self::Csv => ',',
            Self::Tsv => '\t',
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SheetColumn {
    pub name: String,
    pub non_empty: usize,
    pub numeric: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SheetSummary {
    pub input_path: String,
    pub output_path: &'static str,
    pub format: String,
    pub sheet_name: String,
    pub rows: usize,
    pub columns: Vec<SheetColumn>,
    pub preview: Vec<Vec<String>>,
    pub warnings: Vec<String>,
}

pub fn open_sheets(vm: &mut VmInstance) -> Result<SheetSummary, String> {
    vm.mkdir(SHEETS_ROOT, true).map_err(runtime_error)?;
    if vm.read_file(SHEETS_INPUT_PATH).is_err() {
        return import_sheet(vm, "item\tquantity\nexample\t1\n", SheetFormat::Tsv);
    }
    summarize_existing_sheet(vm)
}

pub fn import_sheet(
    vm: &mut VmInstance,
    source: &str,
    format: SheetFormat,
) -> Result<SheetSummary, String> {
    if source.len() > MAX_SHEET_BYTES {
        return Err(format!("sheet exceeds {MAX_SHEET_BYTES} bytes"));
    }
    let summary = summarize_sheet(source, format)?;
    let encoded = serde_json::to_vec_pretty(&summary)
        .map_err(|error| format!("failed to encode sheet summary: {error}"))?;
    vm.mkdir(SHEETS_ROOT, true).map_err(runtime_error)?;
    vm.write_file(SHEETS_INPUT_PATH, source)
        .map_err(runtime_error)?;
    vm.write_file(SHEETS_SUMMARY_PATH, encoded)
        .map_err(runtime_error)?;
    Ok(summary)
}

pub fn summarize_existing_sheet(vm: &mut VmInstance) -> Result<SheetSummary, String> {
    let source = vm.read_text(SHEETS_INPUT_PATH).map_err(runtime_error)?;
    if source.len() > MAX_SHEET_BYTES {
        return Err(format!("sheet exceeds {MAX_SHEET_BYTES} bytes"));
    }
    let format = detect_format(&source)?;
    let summary = summarize_sheet(&source, format)?;
    let encoded = serde_json::to_vec_pretty(&summary)
        .map_err(|error| format!("failed to encode sheet summary: {error}"))?;
    vm.write_file(SHEETS_SUMMARY_PATH, encoded)
        .map_err(runtime_error)?;
    Ok(summary)
}

/// Store an already parsed worksheet from a host-only format reader.
pub fn import_sheet_records(
    vm: &mut VmInstance,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    format: &str,
    sheet_name: String,
    warnings: Vec<String>,
    source_path: &str,
) -> Result<SheetSummary, String> {
    let summary = summarize_records(headers, rows, format, sheet_name, warnings, source_path)?;
    let encoded = serde_json::to_vec_pretty(&summary)
        .map_err(|error| format!("failed to encode sheet summary: {error}"))?;
    vm.mkdir(SHEETS_ROOT, true).map_err(runtime_error)?;
    vm.write_file(SHEETS_SUMMARY_PATH, encoded)
        .map_err(runtime_error)?;
    Ok(summary)
}

fn validate_document(text: &str) -> Result<(), String> {
    if text.len() > MAX_DOCUMENT_BYTES {
        Err(format!("document exceeds {MAX_DOCUMENT_BYTES} bytes"))
    } else {
        Ok(())
    }
}

fn line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count() + usize::from(text.ends_with('\n'))
    }
}

fn detect_format(source: &str) -> Result<SheetFormat, String> {
    let first_row = source
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| "sheet is empty".to_owned())?;
    let tabs = first_row.matches('\t').count();
    let commas = first_row.matches(',').count();
    if tabs == 0 && commas == 0 {
        Err("sheet must be CSV or TSV with at least two columns".to_owned())
    } else if tabs > commas {
        Ok(SheetFormat::Tsv)
    } else {
        Ok(SheetFormat::Csv)
    }
}

fn summarize_sheet(source: &str, format: SheetFormat) -> Result<SheetSummary, String> {
    let records = parse_delimited(source, format.delimiter())?;
    let (headers, rows) = records
        .split_first()
        .ok_or_else(|| "sheet is empty".to_owned())?;
    summarize_records(
        headers.clone(),
        rows.to_vec(),
        format.name(),
        "Sheet1".to_owned(),
        Vec::new(),
        SHEETS_INPUT_PATH,
    )
}

fn summarize_records(
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    format: &str,
    sheet_name: String,
    warnings: Vec<String>,
    source_path: &str,
) -> Result<SheetSummary, String> {
    validate_headers(&headers)?;
    if headers.len() > MAX_SHEET_COLUMNS {
        return Err(format!("sheet has more than {MAX_SHEET_COLUMNS} columns"));
    }
    if rows.len() > MAX_SHEET_ROWS {
        return Err(format!("sheet has more than {MAX_SHEET_ROWS} data rows"));
    }
    let cells = rows
        .iter()
        .try_fold(headers.len(), |count, row| count.checked_add(row.len()))
        .ok_or_else(|| "sheet cell count overflowed".to_owned())?;
    if cells > MAX_SHEET_CELLS {
        return Err(format!("sheet has more than {MAX_SHEET_CELLS} cells"));
    }
    if let Some((index, row)) = rows
        .iter()
        .enumerate()
        .find(|(_, row)| row.len() != headers.len())
    {
        return Err(format!(
            "row {} has {} columns; expected {}",
            index + 2,
            row.len(),
            headers.len()
        ));
    }
    let columns = headers
        .iter()
        .enumerate()
        .map(|(index, name)| SheetColumn {
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
    Ok(SheetSummary {
        input_path: source_path.to_owned(),
        output_path: SHEETS_SUMMARY_PATH,
        format: format.to_owned(),
        sheet_name,
        rows: rows.len(),
        columns,
        preview: rows
            .iter()
            .take(PREVIEW_ROWS)
            .map(|row| row.iter().map(|cell| truncate_cell(cell)).collect())
            .collect(),
        warnings,
    })
}

fn plain_document_metadata() -> DocumentMetadata {
    DocumentMetadata {
        format: "plain_text".to_owned(),
        source_name: None,
        warnings: Vec::new(),
    }
}

fn validate_headers(headers: &[String]) -> Result<(), String> {
    if headers.len() < 2 {
        return Err("sheet must have at least two non-empty column names".to_owned());
    }
    if headers.iter().any(|header| header.trim().is_empty()) {
        return Err("sheet must have non-empty column names".to_owned());
    }
    if headers.iter().collect::<BTreeSet<_>>().len() != headers.len() {
        return Err("sheet column names must be unique".to_owned());
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
                _ => return Err("unexpected text after a quoted sheet cell".to_owned()),
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
        return Err("sheet has an unterminated quoted cell".to_owned());
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
        AppId, DOCS_DOCUMENT_PATH, SHEETS_INPUT_PATH, SHEETS_SUMMARY_PATH, SheetFormat,
        app_descriptors, append_document, import_sheet, open_docs, open_sheets, read_document,
        replace_document,
    };
    use crate::runtime::{ResourceLimits, VmInstance};

    #[test]
    fn descriptors_expose_only_builtin_apps() {
        let descriptors = app_descriptors();
        assert_eq!(descriptors[0].id, AppId::Docs);
        assert_eq!(descriptors[1].id, AppId::Sheets);
        assert!(!descriptors[0].operations.contains(&"exec"));
    }

    #[test]
    fn docs_are_guest_files_with_bounded_text_operations() {
        let mut vm = VmInstance::new("docs");
        assert_eq!(open_docs(&mut vm).unwrap().path, DOCS_DOCUMENT_PATH);
        replace_document(&mut vm, "A/RVM\n").unwrap();
        let document = append_document(&mut vm, "guest document").unwrap();
        assert_eq!(document.text, "A/RVM\nguest document");
        assert_eq!(read_document(&vm).unwrap().lines, 2);
        assert!(vm.read_text(DOCS_DOCUMENT_PATH).is_ok());
    }

    #[test]
    fn sheets_create_a_json_artifact_from_tsv() {
        let mut vm = VmInstance::new("sheets");
        let summary = import_sheet(&mut vm, "name\tamount\nAda\t12\n", SheetFormat::Tsv).unwrap();
        assert_eq!(summary.rows, 1);
        assert_eq!(summary.columns[1].numeric, 1);
        assert_eq!(
            vm.read_text(SHEETS_INPUT_PATH).unwrap(),
            "name\tamount\nAda\t12\n"
        );
        assert!(
            vm.read_text(SHEETS_SUMMARY_PATH)
                .unwrap()
                .contains("amount")
        );
        assert_eq!(open_sheets(&mut vm).unwrap().rows, 1);
    }

    #[test]
    fn sheets_reject_irregular_rows_and_unsupported_shape() {
        let mut vm = VmInstance::new("bad-sheets");
        assert!(import_sheet(&mut vm, "a,b\n1\n", SheetFormat::Csv).is_err());
        assert!(import_sheet(&mut vm, "only\nvalue\n", SheetFormat::Csv).is_err());
    }

    #[test]
    fn app_operations_report_guest_quota_failures() {
        let limits = ResourceLimits {
            max_bytes: 8,
            ..ResourceLimits::default()
        };
        let mut vm = VmInstance::with_limits("quota", limits).unwrap();
        assert!(replace_document(&mut vm, "more than eight bytes").is_err());
    }
}
