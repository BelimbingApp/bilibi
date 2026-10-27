//! A report's whole result as a file, run as the reader: the table every
//! format shares (every page of a list with its totals line, the lines of a
//! summary, or a matrix laid out down and across), then CSV or a text PDF
//! (`bilibi_artifacts::pdf`). Each export is audited with the report, the
//! reader, the format and the row count, never the data. No Bilimbi
//! counterpart.
use crate::{
    cell, contributions, line, web::result, Kind, ReportError, Reports, RunInput, RunResult, EXPORT,
};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::Error;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};

type Outcome<T> = Result<T, ReportError>;

/// Columns a PDF table holds (`pdf::render`).
const PDF_COLUMNS: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Csv,
    Pdf,
}
impl Format {
    pub fn key(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Pdf => "pdf",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "csv" => Some(Self::Csv),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Csv => "text/csv",
            Self::Pdf => "application/pdf",
        }
    }
}

/// A finished export.
#[derive(Debug)]
pub struct File {
    pub filename: String,
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
    /// Result lines in the file (rows, summary lines or matrix rows).
    pub rows: usize,
}

/// The result laid out as one table, whatever the format.
pub(crate) struct Tabled {
    pub name: String,
    pub description: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Result lines, totals lines not counted.
    pub records: usize,
}

impl Reports {
    /// The whole result as a file, within the interactive export cap.
    /// Needs `reports.export`. A larger result is refused with the queued
    /// export as the way on.
    pub async fn export(
        &self,
        scope: &Scope,
        id: i64,
        input: &RunInput,
        format: Format,
    ) -> Outcome<File> {
        let cap = self.export_rows(scope).await?;
        self.export_within(scope, id, input, format, cap, true)
            .await
    }

    /// The same file within `cap` rows; the queued export passes its own. A
    /// scheduled delivery does not `require_export`: its recipient asked for
    /// nothing, and receives only what `reports.view` lets them run.
    pub(crate) async fn export_within(
        &self,
        scope: &Scope,
        id: i64,
        input: &RunInput,
        format: Format,
        cap: usize,
        require_export: bool,
    ) -> Outcome<File> {
        if require_export {
            let client = self.db.get().await.map_err(Error::from)?;
            crate::authorize(&**client, scope, EXPORT).await?;
        }
        let table = self.tabulate(scope, id, input, cap).await?;
        let bytes = render(&table, format)?;
        let client = self.db.get().await.map_err(Error::from)?;
        record_action(
            &**client,
            &Attribution::from_scope(scope),
            None,
            "base.reports.exported",
            json!({"semantic": true, "source": "Reports", "summary": format!("Report exported as {}", format.key().to_uppercase()), "report_id": id, "rows": table.records, "format": format.key()}),
            true,
        )
        .await?;
        Ok(File {
            filename: format!("{}.{}", slug(&table.name), format.key()),
            content_type: format.content_type(),
            bytes,
            rows: table.records,
        })
    }

    /// Runs the report as the reader and lays every line out, within `cap`.
    pub(crate) async fn tabulate(
        &self,
        scope: &Scope,
        id: i64,
        input: &RunInput,
        cap: usize,
    ) -> Outcome<Tabled> {
        // The page's answers, drill-downs and records view; every page.
        let mut input = RunInput {
            page: 0,
            ..input.clone()
        };
        let first = self.run(scope, id, &input).await?;
        let (name, description, kind) = (
            first.report.name.clone(),
            first.report.description.clone(),
            first.report.kind,
        );
        let mut table = Tabled {
            name,
            description,
            headers: Vec::new(),
            rows: Vec::new(),
            records: 0,
        };
        match &first.result {
            RunResult::Rows {
                columns,
                total,
                page_size,
                totals,
                ..
            } => {
                if *total as usize > cap {
                    return Err(too_many(*total, cap));
                }
                table.headers = columns.iter().map(|c| c.label.clone()).collect();
                let totals = totals.clone();
                let pages = (*total as usize).div_ceil((*page_size).max(1));
                let mut current = Some(first);
                for page in 0..pages {
                    let run = match current.take() {
                        Some(run) => run,
                        None => {
                            input.page = page;
                            self.run(scope, id, &input).await?
                        }
                    };
                    if let RunResult::Rows { rows, .. } = run.result {
                        for (_, cells) in rows {
                            table.rows.push(cells.iter().map(cell).collect());
                            table.records += 1;
                        }
                    }
                }
                if let Some(totals) = totals {
                    table.rows.push(
                        totals
                            .iter()
                            .enumerate()
                            .map(|(i, v)| {
                                if i == 0 && v.is_null() {
                                    "Total".into()
                                } else {
                                    cell(v)
                                }
                            })
                            .collect(),
                    );
                }
            }
            RunResult::Summary {
                groupings,
                measures,
                lines,
                total,
                ..
            } if kind == Kind::Matrix && groupings.len() == 2 => {
                let value = |count: i64, values: &[Value]| {
                    if measures.is_empty() {
                        count.to_string()
                    } else {
                        values.first().map(cell).unwrap_or_default()
                    }
                };
                let (mut down, mut across): (Vec<&Value>, Vec<&Value>) = (Vec::new(), Vec::new());
                for (keys, subtotal, _, _) in lines {
                    if *subtotal || keys.len() != 2 {
                        continue;
                    }
                    if !down.contains(&&keys[0]) {
                        down.push(&keys[0]);
                    }
                    if !across.contains(&&keys[1]) {
                        across.push(&keys[1]);
                    }
                }
                table.headers = std::iter::once(groupings[0].0.label.clone())
                    .chain(across.iter().map(|v| result::key_text(v, groupings[1].1)))
                    .chain(std::iter::once("Total".to_owned()))
                    .collect();
                for d in &down {
                    let mut row = vec![result::key_text(d, groupings[0].1)];
                    for a in &across {
                        row.push(
                            lines
                                .iter()
                                .find(|(k, sub, _, _)| {
                                    !*sub && k.first() == Some(*d) && k.get(1) == Some(*a)
                                })
                                .map(|(_, _, count, values)| value(*count, values))
                                .unwrap_or_default(),
                        );
                    }
                    row.push(
                        lines
                            .iter()
                            .find(|(k, sub, _, _)| *sub && k.first() == Some(*d))
                            .map(|(_, _, count, values)| value(*count, values))
                            .unwrap_or_default(),
                    );
                    table.rows.push(row);
                    table.records += 1;
                }
                let mut last = vec!["Total".to_owned()];
                last.extend(across.iter().map(|_| String::new()));
                last.push(value(total.0, &total.1));
                table.rows.push(last);
            }
            RunResult::Summary {
                groupings,
                measures,
                lines,
                total,
                ..
            } => {
                table.headers = groupings
                    .iter()
                    .map(|(c, _)| c.label.clone())
                    .chain(std::iter::once("Records".to_owned()))
                    .chain(
                        measures
                            .iter()
                            .map(|(c, agg)| format!("{} ({})", c.label, result::agg_label(agg))),
                    )
                    .collect();
                for (keys, subtotal, count, values) in lines {
                    let mut fields: Vec<String> = keys
                        .iter()
                        .zip(groupings)
                        .map(|(k, (_, bucket))| result::key_text(k, *bucket))
                        .collect();
                    if *subtotal {
                        if let Some(last) = fields.last_mut() {
                            *last = "Subtotal".into();
                        }
                    }
                    fields.push(count.to_string());
                    fields.extend(values.iter().map(cell));
                    table.rows.push(fields);
                    table.records += 1;
                }
                let mut last: Vec<String> = Vec::new();
                if !groupings.is_empty() {
                    last.push("Total".into());
                    last.extend(std::iter::repeat_n(String::new(), groupings.len() - 1));
                }
                last.push(total.0.to_string());
                last.extend(total.1.iter().map(cell));
                table.rows.push(last);
            }
        }
        Ok(table)
    }

    /// The most rows a queued export holds, for the reader's company.
    pub(crate) async fn queued_rows(&self, scope: &Scope) -> Outcome<usize> {
        let client = self.db.get().await.map_err(Error::from)?;
        Ok(self
            .setting(&**client, scope, contributions::QUEUED_ROWS_KEY)
            .await?)
    }
}

fn too_many(total: i64, cap: usize) -> ReportError {
    ReportError::Refused(format!(
        "This report has {total} rows; a download holds at most {cap}. Prepare it as an export instead, or add a filter."
    ))
}

/// The table in `format`. A PDF holds 20 columns and Latin-1 text; anything
/// else is refused in words rather than altered.
pub(crate) fn render(table: &Tabled, format: Format) -> Outcome<Vec<u8>> {
    match format {
        Format::Csv => {
            let mut out = String::new();
            line(&mut out, table.headers.iter().cloned());
            for row in &table.rows {
                line(&mut out, row.iter().cloned());
            }
            Ok(out.into_bytes())
        }
        Format::Pdf => {
            use bilibi_artifacts::pdf::{self, Block, Document, Metadata};
            if table.headers.len() > PDF_COLUMNS {
                return Err(ReportError::Refused(format!(
                    "A PDF holds at most {PDF_COLUMNS} columns; this result has {}. Download it as CSV instead.",
                    table.headers.len()
                )));
            }
            let mut blocks = Vec::new();
            if !table.description.is_empty() {
                blocks.push(Block::Text(table.description.clone()));
            }
            // Column labels are Grid's own words, whose link mark `›` the
            // PDF's Latin-1 font lacks; the data itself is never altered.
            blocks.push(Block::Table {
                headers: table.headers.iter().map(|h| h.replace('›', ">")).collect(),
                rows: table.rows.clone(),
            });
            pdf::render(&Document {
                title: table.name.clone(),
                metadata: Metadata {
                    subject: Some("Report".into()),
                    ..Metadata::default()
                },
                blocks,
            })
            .map_err(|error| match error {
                bilibi_artifacts::Error::UnsupportedText => ReportError::Refused(
                    "This result holds characters a PDF here cannot print. Download it as CSV instead.".into(),
                ),
                _ => ReportError::Refused("The PDF could not be made. Download it as CSV instead.".into()),
            })
        }
    }
}

/// A file name from the report's name: lower-case words joined by `-`.
pub(crate) fn slug(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "report".into()
    } else {
        slug
    }
}
