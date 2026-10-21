//! The development inbox: the simulator's messages as a phone's message
//! list, newest first, so a tester reads a sign-in or step-up code without a
//! phone. The host serves it at [`crate::INBOX_PATH`] only while the
//! simulator stands in for the transport (the saved instance role is Dev);
//! otherwise the path answers like an unknown one. It is anonymous on
//! purpose: a tester needs the code before signing in.
use crate::{Outbox, Sms, INBOX_PATH};
use askama::Template;
use bilibi_datetime::{DisplayContext, Format, Precision};
use bilibi_ui::{Button, Html, Instant};

struct Item {
    dom_id: String,
    time_id: String,
    to: String,
    body: String,
    code: Option<String>,
    sent: Instant,
}

/// The inbox card body; the host puts it in the credential layout, which is
/// about a phone's width.
#[derive(Template)]
#[template(path = "sms_inbox.html")]
pub struct InboxPage {
    items: Vec<Item>,
    refresh: Html,
}

impl InboxPage {
    /// `None` when the simulator does not stand in for the transport now.
    pub async fn new(sms: &Sms) -> crate::Result<Option<Self>> {
        match sms.outbox().await {
            Some(outbox) => Self::from_outbox(outbox).map(Some),
            None => Ok(None),
        }
    }
    fn from_outbox(outbox: &Outbox) -> crate::Result<Self> {
        let display = DisplayContext::anonymous();
        let mut items = Vec::new();
        for (index, entry) in outbox.entries()?.into_iter().enumerate() {
            let at = jiff::Timestamp::try_from(entry.sent_at).map_err(|_| crate::Error::Invalid)?;
            let rendered = display.render(at, Format::DateTime, Precision::Second);
            let mut sent = Instant::new(
                &rendered.iso,
                &rendered.text,
                &rendered.company,
                &rendered.utc,
                rendered.mode.as_str(),
            );
            sent.precision = "second";
            items.push(Item {
                dom_id: format!("sms-message-{index}"),
                time_id: format!("sms-message-{index}-sent"),
                to: entry.to,
                body: entry.body,
                code: entry.code,
                sent,
            });
        }
        Ok(Self {
            items,
            refresh: Html::render(&Button {
                id: "sms-inbox-refresh",
                href: INBOX_PATH,
                body: Html::text("Refresh"),
                ..Button::default()
            })
            .map_err(|_| crate::Error::Invalid)?,
        })
    }
}
