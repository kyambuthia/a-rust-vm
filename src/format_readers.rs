//! Host-only, read-only format readers for bounded guest upload bytes.
//!
//! Readers receive copied bytes from the guest filesystem and return normalized
//! artifacts. They never receive a host path, a VM handle, a capability, or an
//! executable payload. Macros, formulas, external links, and embedded objects
//! are deliberately inert: visible cached values and document text are all
//! this module exposes.

use std::io::{Cursor, Read};

use calamine::{Reader, open_workbook_auto_from_rs};
use serde::Serialize;
use zip::ZipArchive;

const MAX_ARCHIVE_ENTRIES: usize = 256;
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ARCHIVE_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_EXTRACTED_TEXT_BYTES: usize = 512 * 1024;
const MAX_SHEET_ROWS: usize = 50_000;
const MAX_SHEET_COLUMNS: usize = 256;
const MAX_SHEET_CELLS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DocumentContent {
    pub format: String,
    pub text: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpreadsheetContent {
    pub format: String,
    pub sheet_name: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub warnings: Vec<String>,
}

pub fn read_document(filename: &str, bytes: &[u8]) -> Result<DocumentContent, String> {
    let extension = extension(filename)?;
    match extension.as_str() {
        "txt" | "text" | "md" | "markdown" | "rst" | "html" | "htm" | "xml" => {
            let text = read_utf8(bytes, "document")?;
            bounded_document("plain_text", text, Vec::new())
        }
        "rtf" => bounded_document("rtf", extract_rtf(bytes)?, Vec::new()),
        "docx" | "docm" => {
            let mut archive = bounded_archive(bytes)?;
            let xml = read_archive_part(&mut archive, "word/document.xml")?;
            let mut warnings = Vec::new();
            if extension == "docm" || archive.index_for_name("word/vbaProject.bin").is_some() {
                warnings.push("macro content was detected and ignored".to_owned());
            }
            bounded_document("docx", extract_docx_text(&xml)?, warnings)
        }
        "odt" | "ott" => {
            let mut archive = bounded_archive(bytes)?;
            let xml = read_archive_part(&mut archive, "content.xml")?;
            bounded_document("odt", extract_odt_text(&xml)?, Vec::new())
        }
        "pdf" => Err(
            "PDF opening is metadata-only in this runtime; use /pdf for safe inspection".to_owned(),
        ),
        "doc" | "dot" => {
            Err("legacy DOC is not enabled: it requires a separate OLE parser sandbox".to_owned())
        }
        _ => Err(format!("unsupported document format: .{extension}")),
    }
}

pub fn read_spreadsheet(filename: &str, bytes: &[u8]) -> Result<SpreadsheetContent, String> {
    let extension = extension(filename)?;
    let format = match extension.as_str() {
        "xls" | "xlsx" | "xlsm" | "xlsb" | "xla" | "xlam" | "ods" => extension.as_str(),
        "csv" | "tsv" => return read_delimited_sheet(bytes, &extension),
        _ => return Err(format!("unsupported spreadsheet format: .{extension}")),
    };
    if bytes.starts_with(b"PK\x03\x04") {
        let _ = bounded_archive(bytes)?;
    }
    let mut workbook = open_workbook_auto_from_rs(Cursor::new(bytes.to_vec()))
        .map_err(|error| format!("unable to read {format} workbook: {error}"))?;
    let sheet_name = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| "workbook has no worksheets".to_owned())?;
    let range = workbook
        .worksheet_range(&sheet_name)
        .map_err(|error| format!("unable to read worksheet '{sheet_name}': {error}"))?;
    let rows = range
        .rows()
        .take(MAX_SHEET_ROWS + 1)
        .map(|row| {
            row.iter()
                .take(MAX_SHEET_COLUMNS + 1)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let (headers, rows) = split_sheet_rows(rows)?;
    let mut warnings = Vec::new();
    if matches!(format, "xlsm" | "xlam") {
        warnings
            .push("macro-bearing workbook opened read-only; macros were not executed".to_owned());
    }
    Ok(SpreadsheetContent {
        format: format.to_owned(),
        sheet_name,
        headers,
        rows,
        warnings,
    })
}

fn read_delimited_sheet(bytes: &[u8], format: &str) -> Result<SpreadsheetContent, String> {
    let text = read_utf8(bytes, "spreadsheet")?;
    let delimiter = if format == "tsv" { '\t' } else { ',' };
    let records = parse_delimited(&text, delimiter)?;
    let (headers, rows) = split_sheet_rows(records)?;
    Ok(SpreadsheetContent {
        format: format.to_owned(),
        sheet_name: "Sheet1".to_owned(),
        headers,
        rows,
        warnings: Vec::new(),
    })
}

fn split_sheet_rows(
    mut records: Vec<Vec<String>>,
) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let headers = records
        .first()
        .cloned()
        .ok_or_else(|| "sheet is empty".to_owned())?;
    records.remove(0);
    if headers.len() < 2 || headers.len() > MAX_SHEET_COLUMNS {
        return Err(format!(
            "sheet must have between 2 and {MAX_SHEET_COLUMNS} columns"
        ));
    }
    if headers.iter().any(|header| header.trim().is_empty()) {
        return Err("sheet must have non-empty column names".to_owned());
    }
    if records.len() > MAX_SHEET_ROWS {
        return Err(format!("sheet has more than {MAX_SHEET_ROWS} data rows"));
    }
    let cells = headers
        .len()
        .saturating_add(records.iter().map(Vec::len).sum::<usize>());
    if cells > MAX_SHEET_CELLS {
        return Err(format!("sheet has more than {MAX_SHEET_CELLS} cells"));
    }
    if let Some((index, row)) = records
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
    Ok((headers, records))
}

fn extension(filename: &str) -> Result<String, String> {
    filename
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .filter(|extension| !extension.is_empty())
        .ok_or_else(|| "uploaded file needs an extension".to_owned())
}

fn read_utf8(bytes: &[u8], kind: &str) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| format!("{kind} is not valid UTF-8"))
}

fn bounded_document(
    format: &str,
    text: String,
    warnings: Vec<String>,
) -> Result<DocumentContent, String> {
    if text.len() > MAX_EXTRACTED_TEXT_BYTES {
        return Err(format!(
            "extracted document exceeds {MAX_EXTRACTED_TEXT_BYTES} bytes"
        ));
    }
    Ok(DocumentContent {
        format: format.to_owned(),
        text: normalize_text(&text),
        warnings,
    })
}

fn bounded_archive(bytes: &[u8]) -> Result<ZipArchive<Cursor<Vec<u8>>>, String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes.to_vec()))
        .map_err(|error| format!("invalid ZIP container: {error}"))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "archive has more than {MAX_ARCHIVE_ENTRIES} entries"
        ));
    }
    let mut total = 0u64;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|error| format!("invalid ZIP entry: {error}"))?;
        let name = file.name();
        if name.starts_with('/') || name.contains('\\') || name.split('/').any(|part| part == "..")
        {
            return Err("archive contains an unsafe entry path".to_owned());
        }
        if file.size() > MAX_ARCHIVE_ENTRY_BYTES {
            return Err(format!(
                "archive entry '{name}' exceeds the extraction limit"
            ));
        }
        total = total
            .checked_add(file.size())
            .ok_or_else(|| "archive size overflowed".to_owned())?;
        if total > MAX_ARCHIVE_TOTAL_BYTES {
            return Err(format!(
                "archive exceeds the {MAX_ARCHIVE_TOTAL_BYTES}-byte extraction limit"
            ));
        }
    }
    Ok(archive)
}

fn read_archive_part(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    name: &str,
) -> Result<Vec<u8>, String> {
    let mut file = archive
        .by_name(name)
        .map_err(|_| format!("required archive part is missing: {name}"))?;
    if file.size() > MAX_ARCHIVE_ENTRY_BYTES {
        return Err(format!(
            "archive part '{name}' exceeds the extraction limit"
        ));
    }
    let mut bytes = Vec::with_capacity(file.size() as usize);
    file.by_ref()
        .take(MAX_ARCHIVE_ENTRY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read archive part '{name}': {error}"))?;
    if bytes.len() as u64 > MAX_ARCHIVE_ENTRY_BYTES {
        return Err(format!(
            "archive part '{name}' exceeds the extraction limit"
        ));
    }
    Ok(bytes)
}

fn extract_docx_text(xml: &[u8]) -> Result<String, String> {
    extract_xml_visible_text(xml, "w:t", "w:p")
}

fn extract_odt_text(xml: &[u8]) -> Result<String, String> {
    extract_xml_visible_text(xml, "text:p", "text:p")
}

fn extract_xml_visible_text(
    xml: &[u8],
    text_tag: &str,
    paragraph_tag: &str,
) -> Result<String, String> {
    let xml = read_utf8(xml, "document XML")?;
    let upper = xml.to_ascii_uppercase();
    if upper.contains("<!DOCTYPE") || upper.contains("<!ENTITY") {
        return Err("document XML declarations are not supported".to_owned());
    }
    let mut output = String::new();
    let mut cursor = 0;
    let mut text_depth = 0usize;
    while let Some(relative_start) = xml[cursor..].find('<') {
        let start = cursor + relative_start;
        if text_depth > 0 {
            output.push_str(&decode_xml_entities(&xml[cursor..start])?);
        }
        let end = xml[start..]
            .find('>')
            .map(|offset| start + offset)
            .ok_or_else(|| "document XML contains an unterminated tag".to_owned())?;
        let tag = xml[start + 1..end].trim();
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|character: char| character.is_whitespace() || character == '/')
            .next()
            .unwrap_or_default();
        if !closing && name == text_tag && !tag.ends_with('/') {
            text_depth = text_depth.saturating_add(1);
        } else if closing && name == text_tag {
            text_depth = text_depth.saturating_sub(1);
            if name == paragraph_tag {
                output.push('\n');
            }
        } else if name == "w:tab" || name == "text:tab" {
            output.push('\t');
        } else if name == "w:br" || name == "w:cr" || (closing && name == paragraph_tag) {
            output.push('\n');
        }
        cursor = end + 1;
        if output.len() > MAX_EXTRACTED_TEXT_BYTES {
            return Err(format!(
                "extracted document exceeds {MAX_EXTRACTED_TEXT_BYTES} bytes"
            ));
        }
    }
    Ok(output)
}

fn decode_xml_entities(value: &str) -> Result<String, String> {
    let mut output = value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    if output.contains("&#") {
        return Err("numeric XML entities are not supported".to_owned());
    }
    output.shrink_to_fit();
    Ok(output)
}

fn extract_rtf(bytes: &[u8]) -> Result<String, String> {
    let input = read_utf8(bytes, "RTF document")?;
    if !input.trim_start().starts_with("{\\rtf") {
        return Err("RTF document header is invalid".to_owned());
    }
    let mut output = String::new();
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '{' | '}' => {}
            '\\' => match characters.next() {
                Some(escaped @ ('\\' | '{' | '}')) => output.push(escaped),
                Some('p') => {
                    if characters.by_ref().take(2).collect::<String>() == "ar" {
                        if characters.peek() == Some(&' ') {
                            characters.next();
                        }
                        output.push('\n');
                    } else {
                        skip_rtf_control_word(&mut characters);
                    }
                }
                Some('t') => {
                    if characters.by_ref().take(2).collect::<String>() == "ab" {
                        if characters.peek() == Some(&' ') {
                            characters.next();
                        }
                        output.push('\t');
                    } else {
                        skip_rtf_control_word(&mut characters);
                    }
                }
                Some(_) => {
                    skip_rtf_control_word(&mut characters);
                }
                None => {}
            },
            value => output.push(value),
        }
        if output.len() > MAX_EXTRACTED_TEXT_BYTES {
            return Err(format!(
                "extracted document exceeds {MAX_EXTRACTED_TEXT_BYTES} bytes"
            ));
        }
    }
    Ok(output)
}

fn skip_rtf_control_word(characters: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while matches!(characters.peek(), Some(character) if character.is_ascii_alphabetic() || *character == '-' || character.is_ascii_digit())
    {
        characters.next();
    }
    if characters.peek() == Some(&' ') {
        characters.next();
    }
}

fn normalize_text(text: &str) -> String {
    text.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
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
    use std::io::{Cursor, Write};

    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::{read_document, read_spreadsheet};

    fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn reads_visible_docx_text_and_ignores_macro_payloads() {
        let bytes = archive(&[
            (
                "word/document.xml",
                r#"<w:document><w:body><w:p><w:r><w:t>Hello &amp; welcome</w:t></w:r></w:p><w:p><w:r><w:t>Guest VM</w:t></w:r></w:p></w:body></w:document>"#,
            ),
            ("word/vbaProject.bin", "inert macro bytes"),
        ]);
        let document = read_document("notes.docm", &bytes).unwrap();
        assert_eq!(document.format, "docx");
        assert_eq!(document.text, "Hello & welcome\nGuest VM");
        assert!(document.warnings[0].contains("ignored"));
    }

    #[test]
    fn reads_odt_and_rtf_as_text_without_interpreting_objects() {
        let odt = archive(&[(
            "content.xml",
            r#"<office:document-content><office:body><office:text><text:p>Open <text:span>document</text:span></text:p><text:p>Guest VM</text:p></office:text></office:body></office:document-content>"#,
        )]);
        assert_eq!(
            read_document("notes.odt", &odt).unwrap().text,
            "Open document\nGuest VM"
        );
        assert_eq!(
            read_document("notes.rtf", br#"{\rtf1 Hello\par Guest\tab VM}"#)
                .unwrap()
                .text,
            "Hello\nGuest\tVM"
        );
    }

    #[test]
    fn reads_ods_and_excel_family_workbooks_as_values_not_execution() {
        let bytes = archive(&[
            (
                "[Content_Types].xml",
                r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>name</t></is></c><c r="B1" t="inlineStr"><is><t>amount</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>Ada</t></is></c><c r="B2"><v>12</v></c></row></sheetData></worksheet>"#,
            ),
        ]);
        let sheet = read_spreadsheet("sales.xlsx", &bytes).unwrap();
        assert_eq!(sheet.format, "xlsx");
        assert_eq!(sheet.sheet_name, "Sheet1");
        assert_eq!(sheet.headers, vec!["name", "amount"]);
        assert_eq!(sheet.rows, vec![vec!["Ada", "12"]]);
    }

    #[test]
    fn rejects_unsafe_archive_paths_and_legacy_doc_without_a_parser() {
        let bytes = archive(&[
            ("../outside", "nope"),
            ("word/document.xml", "<w:document/>"),
        ]);
        assert!(read_document("unsafe.docx", &bytes).is_err());
        assert!(read_document("legacy.doc", b"not a CFB document").is_err());
    }
}
