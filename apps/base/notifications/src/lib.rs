//! Notification boundary only: Core User owns persistence and delivery. A
//! business owner records in its own transaction; the boundary publishes to
//! each recipient only after that transaction commits.
use bilibi_database::{Error, MutationTransaction, Result};
use bilibi_security::Scope;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};

/// Recipients one write may notify; a wider audience belongs in a queued job.
pub const MAX_RECIPIENTS: usize = 500;

/// What a recipient sees: Bilimbi's notification `type` and its
/// title/body/url/icon payload. `url` is a local path the bell and
/// `/notifications` open; `icon` is an icon name the renderer may refuse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notice {
    pub kind: String,
    pub title: String,
    pub body: String,
    pub url: Option<String>,
    pub icon: Option<String>,
}
impl Notice {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            kind: "generic".into(),
            title: title.into(),
            body: String::new(),
            url: None,
            icon: None,
        }
    }
    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = kind.into();
        self
    }
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }
    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }
    /// The bounds the `notifications` table also enforces. A link must stay
    /// on this origin: `//host` and `/\host` are other origins to a browser.
    pub fn validate(&self) -> Result<()> {
        let title = self.title.trim();
        let valid = !self.kind.is_empty()
            && self.kind.len() <= 64
            && !title.is_empty()
            && self.title.chars().count() <= 255
            && self.body.chars().count() <= 2000
            && self.url.as_deref().is_none_or(local_path)
            && self
                .icon
                .as_deref()
                .is_none_or(|icon| !icon.is_empty() && icon.len() <= 64);
        if valid {
            Ok(())
        } else {
            Err(Error::Invalid)
        }
    }
}

/// A same-origin absolute path, the only link a notification may carry.
pub fn local_path(url: &str) -> bool {
    url.starts_with('/')
        && !url.starts_with("//")
        && !url.contains('\\')
        && url.len() <= 2048
        && !url.chars().any(char::is_control)
}

pub type RecordFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
pub trait NotificationSink: Send + Sync {
    /// Persist one notice per recipient inside `tx`, refusing any recipient
    /// that is not an active account of the transaction's tenant.
    fn record<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        recipients: &'a [i64],
        notice: &'a Notice,
    ) -> RecordFuture<'a>;
    /// Called once after commit; delivery wakes only these recipients.
    fn committed(&self, tenant: i64, recipients: &[i64]);
    /// Which of `recipients` `record` would accept now in the transaction's
    /// tenant, so a system worker can tell the people still here without
    /// acting as them. The default offers them all; `record` still refuses.
    fn eligible<'a>(
        &'a self,
        _tx: &'a MutationTransaction<'_>,
        recipients: &'a [i64],
    ) -> EligibleFuture<'a> {
        Box::pin(async move { Ok(recipients.to_vec()) })
    }
}
pub type EligibleFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<i64>>> + Send + 'a>>;

/// Bind exactly once during service installation. An unbound sink fails boot;
/// callers never silently lose a notification or commit without its audit.
#[derive(Default)]
pub struct NotificationBoundary {
    sink: OnceLock<Arc<dyn NotificationSink>>,
}
impl NotificationBoundary {
    pub fn bind(&self, sink: Arc<dyn NotificationSink>) -> bilibi_module_registry::Result<()> {
        self.sink.set(sink).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "duplicate notification sink binding".into(),
            )
        })
    }
    fn sink(&self) -> bilibi_module_registry::Result<&dyn NotificationSink> {
        self.sink.get().map(|sink| sink.as_ref()).ok_or_else(|| {
            bilibi_module_registry::RegistryError::Service("unbound notification sink".into())
        })
    }
    /// The recipients among `recipients` that can be notified now, sorted
    /// and without repeats.
    pub async fn eligible(
        &self,
        tx: &MutationTransaction<'_>,
        recipients: &[i64],
    ) -> Result<Vec<i64>> {
        let mut wanted: Vec<i64> = recipients
            .iter()
            .copied()
            .filter(|user| *user > 0)
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.is_empty() {
            return Ok(wanted);
        }
        self.sink()?.eligible(tx, &wanted).await
    }
    /// Tells the acting account about its own write.
    pub async fn record(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        title: &str,
    ) -> Result<()> {
        self.notify(tx, scope, &[scope.user()], &Notice::new(title))
            .await
    }
    /// Notifies each recipient once, in the caller's transaction. Nothing is
    /// delivered unless and until that transaction commits.
    pub async fn notify(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        recipients: &[i64],
        notice: &Notice,
    ) -> Result<()> {
        notice.validate()?;
        let mut recipients = recipients.to_vec();
        recipients.sort_unstable();
        recipients.dedup();
        if recipients.is_empty()
            || recipients.len() > MAX_RECIPIENTS
            || recipients.iter().any(|user| *user <= 0)
        {
            return Err(Error::Invalid);
        }
        let sink = self.sink.get().cloned().ok_or_else(|| {
            bilibi_module_registry::RegistryError::Service("unbound notification sink".into())
        })?;
        sink.record(tx, scope, &recipients, notice).await?;
        let tenant = scope.tenant();
        tx.after_commit(move || sink.committed(tenant, &recipients));
        Ok(())
    }
}
inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/notifications", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/security"],
        migrations: &[],
        services: &[bilibi_module_registry::Service { id: "base/notifications.sink", factory: |_| Ok(Arc::new(NotificationBoundary::default())) }],
    }
}
inventory::submit! {
    bilibi_module_registry::Ready {
        module: "base/notifications",
        check: |services| services.get::<NotificationBoundary>("base/notifications.sink")?.sink().map(|_| ()),
    }
}
