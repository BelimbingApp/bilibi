//! A GeoNames import run from a screen, in the background: the operator's
//! Update on the Countries and Admin1 Divisions screens (Bilimbi
//! `CountriesLive` `update-countries`; Belimbing
//! `Geonames\Livewire\Admin1\Index::update`) and the Postcodes screen's
//! Import and Update (Belimbing `Geonames\Livewire\Postcodes\Index::{import,
//! update}`). `Run` imports its targets one at a time, wakes the page after
//! each so it can say which one is running and show the rows so far, and
//! leaves the finished run's outcomes for the page to read as its flash.
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

/// One target's result: its report, or why it was not imported.
pub(crate) struct Outcome {
    pub target: Target,
    pub result: std::result::Result<DatasetReport, ImportError>,
}

/// Where a run is: how many targets are done, of how many, and which one
/// is being imported now.
#[derive(Clone)]
pub(crate) struct Progress {
    pub done: usize,
    pub total: usize,
    pub current: Option<Target>,
}

#[derive(Default)]
struct Shared {
    total: usize,
    current: Option<Target>,
    outcomes: Vec<Outcome>,
    finished: bool,
}

/// What `start` did.
pub(crate) enum Started {
    Ok,
    /// A run is already going; the press is ignored (Bilimbi
    /// `updating_countries?`).
    Busy,
    /// The actor no longer holds `admin.geonames.update`.
    Denied,
}

/// What `settle` found: the number of outcomes the page has not yet seen
/// (the rows changed; read them again), and the whole run once it finished.
pub(crate) struct Settled {
    pub new_outcomes: usize,
    pub finished: Option<Vec<Outcome>>,
}

pub(crate) struct Run {
    busy: bool,
    /// How many outcomes the page has already seen.
    seen: usize,
    shared: Arc<Mutex<Shared>>,
    wake: broadcast::Sender<Change>,
}

impl Run {
    pub fn new() -> Self {
        let (wake, _) = broadcast::channel(8);
        Self {
            busy: false,
            seen: 0,
            shared: Arc::new(Mutex::new(Shared::default())),
            wake,
        }
    }

    pub fn subscription(&self) -> Subscription {
        Subscription {
            receiver: self.wake.subscribe(),
        }
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn progress(&self) -> Option<Progress> {
        if !self.busy {
            return None;
        }
        let shared = self.lock();
        Some(Progress {
            done: shared.outcomes.len(),
            total: shared.total,
            current: shared.current.clone(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Reads what the background run has done since the page last looked.
    pub fn settle(&mut self) -> Settled {
        if !self.busy {
            return Settled {
                new_outcomes: 0,
                finished: None,
            };
        }
        let (seen, finished) = {
            let mut shared = self
                .shared
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let seen = shared.outcomes.len();
            let finished = shared.finished.then(|| {
                shared.finished = false;
                shared.current = None;
                std::mem::take(&mut shared.outcomes)
            });
            (seen, finished)
        };
        let new_outcomes = seen.saturating_sub(self.seen);
        self.seen = seen;
        if finished.is_some() {
            self.busy = false;
            self.seen = 0;
        }
        Settled {
            new_outcomes,
            finished,
        }
    }

    /// The page's `can_update` only shows the control. This asks again, and
    /// each target's import asks with the sealed scope, so a grant revoked
    /// after mount (or during the run) imports nothing more.
    pub async fn start(
        &mut self,
        pages: &Pages,
        scope: &Scope,
        targets: Vec<Target>,
    ) -> Result<Started> {
        if self.busy {
            return Ok(Started::Busy);
        }
        let client = pages.db.get().await?;
        match bilibi_authz::authorize_key(&**client, scope, UPDATE).await {
            Ok(()) => {}
            Err(Error::Forbidden) => return Ok(Started::Denied),
            Err(error) => return Err(error),
        }
        drop(client);
        self.busy = true;
        self.seen = 0;
        *self.lock() = Shared {
            total: targets.len(),
            ..Shared::default()
        };
        let importer = pages.importer.clone();
        let scope = scope.clone();
        let tenant = scope.tenant();
        let shared = self.shared.clone();
        let wake = self.wake.clone();
        tokio::spawn(async move {
            let options = ImportOptions::default();
            let lock = || {
                shared
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            };
            for target in targets {
                lock().current = Some(target.clone());
                let _ = wake.send(Change { tenant, user: None });
                let result = importer.import_as(&scope, target.clone(), &options).await;
                let denied = matches!(result, Err(ImportError::Access(Error::Forbidden)));
                lock().outcomes.push(Outcome { target, result });
                if denied {
                    break;
                }
            }
            lock().finished = true;
            let _ = wake.send(Change { tenant, user: None });
        });
        Ok(Started::Ok)
    }

    /// A header button that spins while the run goes.
    pub fn button(
        &self,
        id: &'static str,
        event: &'static str,
        icon: &'static str,
        variant: ButtonVariant,
        label: &str,
        working: &str,
    ) -> Result<Html> {
        let icon = render(Icon {
            name: icon,
            class: if self.busy {
                "size-4 animate-spin"
            } else {
                "size-4"
            },
        })?;
        let label = if self.busy { working } else { label };
        render(Button {
            variant,
            id,
            button_type: "button",
            event,
            disabled: self.busy,
            body: Html::raw(format!("{icon}<span>{}</span>", Html::text(label))),
            ..Button::default()
        })
    }
}

/// What one screen's Update refreshes and how it names it.
pub(crate) struct Spec {
    pub dataset: Dataset,
    /// The plural the messages open with: "Countries", "Admin1 divisions".
    pub noun: &'static str,
    pub button_id: &'static str,
    pub event: &'static str,
    pub denied: &'static str,
}

/// The Countries and Admin1 screens' Update: one dataset, one report.
pub(crate) struct Update {
    spec: Spec,
    run: Run,
}

impl Update {
    pub fn new(spec: Spec) -> Self {
        Self {
            spec,
            run: Run::new(),
        }
    }

    pub fn subscription(&self) -> Subscription {
        self.run.subscription()
    }

    /// Reads the background import's report, once it has one. True when the
    /// rows changed and the page should read them again.
    pub fn settle(&mut self, flash: &mut Flash) -> bool {
        let Some(outcomes) = self.run.settle().finished else {
            return false;
        };
        let noun = self.spec.noun;
        match outcomes.into_iter().next().map(|outcome| outcome.result) {
            Some(Ok(report)) => {
                let (kind, message) = dataset_message(noun, &report);
                flash.put(kind, message);
                true
            }
            Some(Err(ImportError::Access(Error::Forbidden))) => {
                flash.put(StatusKind::Error, self.spec.denied);
                false
            }
            Some(Err(error)) => {
                flash.put(
                    StatusKind::Error,
                    format!("{noun} were not changed. {}", not_changed_advice(&error)),
                );
                false
            }
            None => false,
        }
    }

    pub async fn start(&mut self, pages: &Pages, scope: &Scope, flash: &mut Flash) -> Result<()> {
        if let Started::Denied = self
            .run
            .start(pages, scope, vec![Target::Dataset(self.spec.dataset)])
            .await?
        {
            flash.put(StatusKind::Error, self.spec.denied);
        }
        Ok(())
    }

    /// The header's Update control, spinning while the import runs.
    pub fn button(&self) -> Result<Html> {
        self.run.button(
            self.spec.button_id,
            self.spec.event,
            "refresh",
            ButtonVariant::Primary,
            "Update",
            "Updating…",
        )
    }
}

/// A fallback is NOT an update: the download failed and the existing
/// local file was reused, so the operator must not read "updated"
/// (Bilimbi #273).
fn dataset_message(noun: &str, report: &DatasetReport) -> (StatusKind, String) {
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
                source(report),
                report.imported,
                report.skipped
            ),
        ),
    }
}

/// Where a successful report's rows came from, for a message.
pub(crate) fn source(report: &DatasetReport) -> &'static str {
    if report.cached() {
        "the current local GeoNames download"
    } else {
        "a fresh GeoNames download"
    }
}

/// Why nothing changed and what to do next, as the sentence after
/// "<Something> were not changed." Written for the operator reading a
/// flash, so it names the likely cause and the next step.
pub(crate) fn not_changed_advice(error: &ImportError) -> String {
    match error {
        ImportError::Download(_, DownloadFailure::Request(_)) => {
            "Bilibi could not connect to download.geonames.org. Check internet, proxy, or firewall access, then try again. If it persists, contact your administrator.".into()
        }
        ImportError::Download(_, DownloadFailure::HttpStatus(404)) => {
            "GeoNames has no postcode file for this country (HTTP 404).".into()
        }
        ImportError::Download(_, DownloadFailure::HttpStatus(status)) => format!(
            "GeoNames responded with HTTP {status}. Try again later; if it persists, contact your administrator."
        ),
        ImportError::Import(..) | ImportError::Extract(..) => {
            "GeoNames data was downloaded but could not be imported, and the existing data was kept. Try again; if it persists, contact your administrator.".into()
        }
        _ => "The GeoNames download did not finish. Try again; if it persists, contact your administrator.".into(),
    }
}

/// A 503 is not the same as an unplugged cable: telling an operator to check
/// their firewall when GeoNames is simply down wastes their afternoon.
pub(crate) fn fallback_cause(cause: &FallbackCause) -> String {
    match cause {
        FallbackCause::Unreachable(_) => "GeoNames could not be reached".into(),
        FallbackCause::HttpStatus(status) => {
            format!("GeoNames returned an error (HTTP {status})")
        }
    }
}
pub(crate) fn as_of(cached_at: Option<SystemTime>) -> String {
    cached_at
        .and_then(|at| jiff::Timestamp::try_from(at).ok())
        .map(|at| format!(", so this data is from {}", at.strftime("%d %b %Y")))
        .unwrap_or_default()
}
