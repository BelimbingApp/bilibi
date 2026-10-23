//! `/reports/{id}/export`: the report's whole result as CSV, or as a PDF with
//! `format=pdf`, run as the reader with the answers the address carries
//! (`a<i>`, as the Result tab writes them). The facade checks
//! `reports.export`, the download bound and audits the export.
use super::{page_error, show};
use crate::Reports;
use bilibi_database::Error;
use bilibi_tenancy::Scope;
use bilibi_ui::download::{Download, DownloadFuture, DownloadRequest, FileBody};
use std::sync::Arc;

pub(crate) struct Csv {
    pub reports: Arc<Reports>,
}

impl Download<Scope, Error> for Csv {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let id = bilibi_ui::params::positive_id(request.path.get("id").map(String::as_str))
                .ok_or(Error::NotFound)?;
            let report = self
                .reports
                .get(request.scope, id)
                .await
                .map_err(page_error)?;
            let answers = show::answers(&report, &show::answer_keys(request.query));
            let format = match request.query.get("format").map(String::as_str) {
                None | Some("csv") => crate::Format::Csv,
                Some("pdf") => crate::Format::Pdf,
                Some(_) => return Err(Error::NotFound),
            };
            let file = self
                .reports
                .export(request.scope, id, &answers, format)
                .await
                .map_err(refused)?;
            Ok(FileBody::attachment(
                file.bytes,
                file.content_type,
                &file.filename,
            ))
        })
    }
}

/// A run the reader may not export (too many rows, a hidden filter) is a
/// refusal of this file, not a missing one.
fn refused(error: crate::ReportError) -> Error {
    match error {
        crate::ReportError::NotFound
        | crate::ReportError::Forbidden
        | crate::ReportError::Unreadable
        | crate::ReportError::Database(_) => page_error(error),
        _ => Error::Forbidden,
    }
}

/// `/report-exports/{id}`: a prepared export, for the person who asked for
/// it, until it expires.
pub(crate) struct Prepared {
    pub reports: Arc<Reports>,
}

impl Download<Scope, Error> for Prepared {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let id = bilibi_ui::params::positive_id(request.path.get("id").map(String::as_str))
                .ok_or(Error::NotFound)?;
            let file = self
                .reports
                .exported(request.scope, id)
                .await
                .map_err(refused)?;
            Ok(FileBody::attachment(
                file.bytes,
                file.content_type,
                &file.filename,
            ))
        })
    }
}
