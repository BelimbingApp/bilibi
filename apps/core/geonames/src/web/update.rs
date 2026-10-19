//! The operator's Update on the Countries and Admin1 Divisions screens
//! (Bilimbi `CountriesLive` `update-countries`; Belimbing
//! `Geonames\Livewire\Admin1\Index::update`): refreshes one GeoNames dataset
//! in the background and reports what it did as the page's flash.
use super::*;
use crate::{
    Dataset, DatasetReport, DownloadFailure, DownloadStatus, FallbackCause, ImportError,
    ImportOptions, Target,
};
use bilibi_ui::{
    page::{Change, Subscription},
    Button, ButtonVariant, Icon,
};
use std::sync::Mutex;
use tokio::sync::broadcast;

type Outcome = std::result::Result<Vec<DatasetReport>, ImportError>;

/// What one screen's Update refreshes and how it names it.
pub(crate) struct Spec {
    pub dataset: Dataset,
    /// The plural the messages open with: "Countries", "Admin1 divisions".
    pub noun: &'static str,
    pub button_id: &'static str,
    pub event: &'static str,
    pub denied: &'static str,
}

pub(crate) struct Update {
    spec: Spec,
    /// An Update is running (Bilimbi `updating_countries?`): the control is
    /// disabled and a second press is ignored until it reports.
    updating: bool,
    /// Where the background import leaves its report; `wake` then asks the
    /// host for a refresh, which reads it.
    outcome: Arc<Mutex<Option<Outcome>>>,
    wake: broadcast::Sender<Change>,
}

impl Update {
    pub fn new(spec: Spec) -> Self {
        let (wake, _) = broadcast::channel(4);
        Self {
            spec,
            updating: false,
            outcome: Arc::new(Mutex::new(None)),
            wake,
        }
    }

    pub fn subscription(&self) -> Subscription {
        Subscription {
            receiver: self.wake.subscribe(),
        }
    }

    /// Reads the background import's report, once it has one. True when the
    /// rows changed and the page should read them again.
    pub fn settle(&mut self, flash: &mut Flash) -> bool {
        let outcome = self
            .outcome
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(outcome) = outcome else {
            return false;
        };
        self.updating = false;
        match outcome {
            Ok(reports) => {
                let (kind, message) = self.message(&reports);
                flash.put(kind, message);
                true
            }
            Err(ImportError::Access(Error::Forbidden)) => {
                flash.put(StatusKind::Error, self.spec.denied);
                false
            }
            Err(error) => {
                flash.put(StatusKind::Error, self.error_message(&error));
                false
            }
        }
    }

    /// The page's `can_update` only shows the control. This asks again, and
    /// the import asks with the sealed scope, so a grant revoked after mount
    /// does not refresh anything.
    pub async fn start(&mut self, pages: &Pages, scope: &Scope, flash: &mut Flash) -> Result<()> {
        if self.updating {
            return Ok(());
        }
        let client = pages.db.get().await?;
        match bilibi_authz::authorize_key(&**client, scope, UPDATE).await {
            Ok(()) => {}
            Err(Error::Forbidden) => {
                flash.put(StatusKind::Error, self.spec.denied);
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        drop(client);
        self.updating = true;
        let importer = pages.importer.clone();
        let dataset = self.spec.dataset;
        let scope = scope.clone();
        let tenant = scope.tenant();
        let outcome = self.outcome.clone();
        let wake = self.wake.clone();
        tokio::spawn(async move {
            let options = ImportOptions {
                datasets: vec![dataset],
                ..ImportOptions::default()
            };
            let report = importer.run_as(&scope, &options).await;
            *outcome
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(report);
            let _ = wake.send(Change { tenant, user: None });
        });
        Ok(())
    }

    /// The header's Update control, spinning while the import runs.
    pub fn button(&self) -> Result<Html> {
        let label = if self.updating {
            "Updating…"
        } else {
            "Update"
        };
        let icon = render(Icon {
            name: "refresh",
            class: if self.updating {
                "size-4 animate-spin"
            } else {
                "size-4"
            },
        })?;
        render(Button {
            variant: ButtonVariant::Primary,
            id: self.spec.button_id,
            button_type: "button",
            event: self.spec.event,
            disabled: self.updating,
            body: Html::raw(format!("{icon}<span>{label}</span>")),
            ..Button::default()
        })
    }

    /// A fallback is NOT an update: the download failed and the existing
    /// local file was reused, so the operator must not read "updated"
    /// (Bilimbi #273).
    fn message(&self, reports: &[DatasetReport]) -> (StatusKind, String) {
        let noun = self.spec.noun;
        let Some(report) = reports
            .iter()
            .find(|report| report.target == Target::Dataset(self.spec.dataset))
        else {
            return (
                StatusKind::Success,
                format!("{noun} updated from GeoNames."),
            );
        };
        match &report.download {
            DownloadStatus::Fallback(cause) => (
                StatusKind::Warning,
                format!(
                    "{noun} were not updated: {}{}. Kept the existing local data ({} imported, {} skipped). Try Update again later.",
                    fallback_cause(cause),
                    as_of(report.cached_at),
                    report.imported,
                    report.skipped
                ),
            ),
            _ => (
                StatusKind::Success,
                format!(
                    "{noun} updated from {}: {} imported, {} skipped.",
                    if report.cached() {
                        "the current local GeoNames download"
                    } else {
                        "a fresh GeoNames download"
                    },
                    report.imported,
                    report.skipped
                ),
            ),
        }
    }

    fn error_message(&self, error: &ImportError) -> String {
        let noun = self.spec.noun;
        match error {
            ImportError::Download(_, DownloadFailure::Request(_)) => format!(
                "{noun} were not changed. Bilibi could not connect to download.geonames.org. Check internet, proxy, or firewall access, then try Update again. If it persists, contact your administrator."
            ),
            ImportError::Download(_, DownloadFailure::HttpStatus(status)) => format!(
                "{noun} were not changed. GeoNames responded with HTTP {status}. Try Update again later; if it persists, contact your administrator."
            ),
            ImportError::Import(..) | ImportError::Extract(..) => format!(
                "{noun} were not changed. GeoNames data was downloaded but could not be imported, and the existing data was kept. Try Update again; if it persists, contact your administrator."
            ),
            _ => format!(
                "{noun} were not changed because the GeoNames update did not finish. Try Update again; if it persists, contact your administrator."
            ),
        }
    }
}

/// A 503 is not the same as an unplugged cable: telling an operator to check
/// their firewall when GeoNames is simply down wastes their afternoon.
fn fallback_cause(cause: &FallbackCause) -> String {
    match cause {
        FallbackCause::Unreachable(_) => "GeoNames could not be reached".into(),
        FallbackCause::HttpStatus(status) => {
            format!("GeoNames returned an error (HTTP {status})")
        }
    }
}
fn as_of(cached_at: Option<SystemTime>) -> String {
    cached_at
        .and_then(|at| jiff::Timestamp::try_from(at).ok())
        .map(|at| format!(", so this data is from {}", at.strftime("%d %b %Y")))
        .unwrap_or_default()
}
