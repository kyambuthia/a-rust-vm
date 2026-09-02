# Document and spreadsheet reader roadmap

This roadmap turns the Docs and Sheets apps into a safe, read-only import
surface without widening the VM's authority. It is a format-reader roadmap,
not an agreement to execute macros, formulas, embedded code, or host tools.

## Contract

1. An upload remains raw bytes under the caller's session-local
   `/workspace/uploads` directory.
2. The server copies those bytes, releases the guest-VM lock, then runs a pure
   bounded reader. A reader never receives a host path, VM handle, capability,
   network client, clock, or subprocess.
3. Only normalized text or cached cell values return to the guest app. The raw
   upload remains opaque to text tools.
4. The result is written as a normal Docs document or Sheets JSON summary and
   recorded in the existing session-local job log.

## Current implementation

| Family | Extensions | Result | Policy |
| --- | --- | --- | --- |
| Plain text | `txt`, `text`, `md`, `markdown`, `rst`, `html`, `htm`, `xml` | UTF-8 text | bounded text only |
| Rich text | `rtf` | visible text | control words and embedded objects are skipped |
| Word OOXML | `docx`, `docm` | document-order visible text | macro payloads are detected, tagged, and never run |
| OpenDocument text | `odt`, `ott` | visible paragraph text | read-only |
| Delimited sheets | `csv`, `tsv` | validated table summary | bounded rows, columns, and cells |
| Excel/OpenDocument sheets | `xls`, `xlsx`, `xlsm`, `xlsb`, `xla`, `xlam`, `ods` | first worksheet's cached cell values | read-only; macros/formulas are not executed |

Use the existing upload API to place a file in the guest VM, then open it from
the unchanged terminal UI:

```text
/docs open proposal.docx
/sheets open forecast.xlsx
```

## Explicitly refused today

- Password-protected or encrypted Office/OpenDocument files.
- Macro, VBA, XLM, DDE, PDF JavaScript, ActiveX, embedded OLE, or external-link
  execution.
- Host office suites, conversion binaries, shell commands, and network fetches.
- Legacy Word `doc`/`dot` content parsing. This requires a dedicated bounded
  OLE/CFB parser and remains a separate implementation phase.
- PDF text extraction. `/pdf` remains a metadata-only safety check.
- Spreadsheet editing or OOXML/ODF round-trip serialization.

Every refused case returns a named error. No format silently falls back to a
host parser or a model claim.

## Delivery sequence

- [x] P0: session-local app artifacts, CSV/TSV validation, job audit records.
- [x] P1: host-only format-reader module, bounded ZIP preflight, DOCX/DOCM,
  ODT, RTF, and Calamine-backed workbook import.
- [ ] P2: content-type/magic conflict reporting, multi-sheet selection, flat
  ODF, and shared parsing policy for the older tabulation job.
- [ ] P3: a separately reviewed OLE/CFB reader for legacy DOC, plus a bounded
  PDF text-extraction project.
- [ ] P4: fuzz/property tests for ZIP/XML/RTF/BIFF inputs, corpus regression
  fixtures, and an isolated WASI parser tier for untrusted high-complexity
  formats.

## Sources and implementation choices

Calamine is a pure-Rust, read-only reader for Excel and OpenDocument spreadsheet
families, including XLS, XLSX, XLSM, XLSB, add-ins, and ODS. Its read-only model
fits this project because A/RVM imports cached values rather than calculating or
executing workbook content. ZIP-backed OOXML/ODF is preflighted independently
before parser invocation to bound entry count and decompressed size.

- Calamine project and supported formats: <https://github.com/tafia/calamine>
- Calamine API: <https://docs.rs/calamine/0.36.1/calamine/>
- ZIP crate API: <https://docs.rs/zip/8.6.0/zip/>
