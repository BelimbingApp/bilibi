//! An owner's addresses (Bilimbi `AddressesPanel`), the discovered embed
//! `company.addresses`/`person.addresses` that Company and Person detail
//! mount with the owner's ID. The owner is a field of the panel, not a second
//! component: its capability, noun and DOM-id prefix come from `Profile`.
//!
//! Showing a control is presentation; every write goes through
//! `AddressService`, which reauthorizes and refuses an archived company or a
//! retired/system Person. Outcomes render as the panel's own notice, inside an
//! open dialog while one is open, because the page behind a modal is inert.
mod events;
mod forms;
mod view;

use super::{location::LocationDraft, *};
use crate::{AddressInput, AttachedAddress, FieldError, Location};
use bilibi_ui::{Choice, ListColumns, NoticeKind, SortDir};

const COLUMNS: [(&str, &str); 7] = [
    ("label", "Label"),
    ("line1", "Address"),
    ("kind", "Kind"),
    ("is_primary", "Primary"),
    ("priority", "Priority"),
    ("valid_from", "Valid From"),
    ("valid_to", "Valid To"),
];

/// What differs between a company's panel and a person's.
struct Profile {
    capability: &'static str,
    prefix: &'static str,
    noun: &'static str,
    caption: &'static str,
    query: &'static str,
    forbidden: &'static str,
    empty_manage: &'static str,
    empty_read: &'static str,
    empty_read_only: &'static str,
}
fn profile(owner: PartyId) -> Profile {
    match owner {
        PartyId::Organisation(_) => Profile {
            capability: "admin.company.update",
            prefix: "company",
            noun: "company",
            caption: "Company addresses",
            query: "company",
            forbidden: "You do not have permission to edit companies.",
            empty_manage: "Attach an existing address, or create one and attach it.",
            empty_read: "Ask someone who can edit companies to attach one.",
            empty_read_only:
                "This company is archived and read-only, so no address can be attached.",
        },
        PartyId::Person(_) => Profile {
            capability: "admin.person.update",
            prefix: "person",
            noun: "person",
            caption: "Person addresses",
            query: "person",
            forbidden: "You do not have permission to edit people.",
            empty_manage: "Attach an existing address, or create one and attach it.",
            empty_read: "Ask someone who can edit people to attach one.",
            empty_read_only: "This person is retired and read-only, so no address can be attached.",
        },
    }
}

/// The attach dialog's draft and refusal.
#[derive(Default)]
struct AttachDraft {
    error: String,
}
/// The create-and-attach dialog's draft (Bilimbi's company inline create).
struct CreateDraft {
    input: AddressInput,
    location: LocationDraft,
    kinds: Vec<crate::AddressKind>,
    primary: bool,
    priority: String,
    errors: Vec<FieldError>,
}
/// The attachment an unlink confirmation names and acts on.
struct PendingDetach {
    attachment: crate::AttachmentId,
    version: i64,
    label: Option<String>,
    code: String,
}

pub(super) struct Panel {
    pages: Pages,
    owner: PartyId,
    profile: Profile,
    anchor: String,
    columns: ListColumns,
    sort_by: String,
    sort_dir: SortDir,
    attached: Vec<AttachedAddress>,
    available: Vec<crate::Address>,
    countries: Vec<Choice>,
    can_manage: bool,
    read_only: bool,
    notice: Option<(NoticeKind, String)>,
    attach: Option<AttachDraft>,
    create: Option<CreateDraft>,
    editing_kinds: Option<AddressId>,
    pending: Option<PendingDetach>,
    statuses: bilibi_ui::CommitStates,
}
impl Panel {
    pub async fn new(pages: Pages, scope: &Scope, owner: PartyId, anchor: String) -> Result<Self> {
        let columns = COLUMNS
            .into_iter()
            .map(|(id, label)| {
                let column = bilibi_ui::BuiltinColumn::new(id, label).sortable(id);
                if id == "priority" {
                    column.right()
                } else {
                    column
                }
            })
            .collect();
        let mut panel = Self {
            pages,
            owner,
            profile: profile(owner),
            anchor,
            columns: ListColumns::mount("addresses-table", columns).map_err(|_| Error::Internal)?,
            sort_by: "label".into(),
            sort_dir: SortDir::Asc,
            attached: Vec::new(),
            available: Vec::new(),
            countries: Vec::new(),
            can_manage: false,
            read_only: false,
            notice: None,
            attach: None,
            create: None,
            editing_kinds: None,
            pending: None,
            statuses: bilibi_ui::CommitStates::default(),
        };
        panel.reload(scope).await?;
        Ok(panel)
    }
    /// An archived company or a retired/system Person keeps its addresses
    /// readable and refuses every change.
    async fn owner_read_only(&self, scope: &Scope) -> Result<bool> {
        let client = self.pages.db.get().await?;
        Ok(match self.owner {
            PartyId::Organisation(id) => matches!(
                bilibi_company::organisation::require_writable(&**client, scope, id.get(), false)
                    .await,
                Err(bilibi_company::WriteError::Archived)
            ),
            PartyId::Person(id) => {
                let person = bilibi_person::identity::read(&**client, scope, id).await?;
                person.retired_at.is_some() || person.system_key.is_some()
            }
        })
    }
    async fn reload(&mut self, scope: &Scope) -> Result<()> {
        self.read_only = self.owner_read_only(scope).await?;
        self.can_manage =
            !self.read_only && self.pages.allowed(scope, self.profile.capability).await?;
        self.attached = self.pages.addresses.attached(scope, self.owner).await?;
        sort(&mut self.attached, &self.sort_by, self.sort_dir);
        self.columns
            .load(&self.attached, |a| a.address.id.to_string());
        Ok(())
    }
    fn listed(&self, id: Option<&str>) -> Option<&AttachedAddress> {
        let id = bilibi_ui::params::positive_id(id)?;
        self.attached.iter().find(|a| a.address.id.get() == id)
    }
    fn notice(&mut self, kind: NoticeKind, message: impl Into<String>) {
        self.notice = Some((kind, message.into()));
    }
    /// A write the owner refused: say so, and stop offering controls.
    fn forbidden(&mut self) {
        self.can_manage = false;
        self.notice(NoticeKind::Error, self.profile.forbidden);
    }
    /// The sentence for a refused attachment write.
    fn refusal(&self, error: &WriteError, fallback: &str) -> String {
        match error {
            WriteError::Archived | WriteError::OwnerReadOnly => self.profile.empty_read_only.into(),
            WriteError::AlreadyAttached => {
                format!(
                    "This address is already attached to this {}.",
                    self.profile.noun
                )
            }
            WriteError::Stale => {
                "The address link changed while you were editing it. Review it and try again."
                    .into()
            }
            WriteError::Access(Error::Forbidden) => self.profile.forbidden.into(),
            _ => fallback.into(),
        }
    }
}
impl LiveState<Scope, Error> for Panel {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Event(value) => {
                    self.handle(request.scope, request.credential, value)
                        .await?
                }
                LiveInput::Refresh | LiveInput::Params(_) => self.reload(request.scope).await?,
            }
            Ok(LiveBody {
                html: self.view(request.scope).await?,
                rows: self.attached.len(),
                ..LiveBody::default()
            })
        })
    }
}

/// The panel's own order over the small attached set (Bilimbi
/// `sort_addresses/3`), ties by address ID.
fn sort(addresses: &mut [AttachedAddress], by: &str, dir: SortDir) {
    addresses.sort_by(|a, b| {
        let (x, y) = (&a.attachment.attachment, &b.attachment.attachment);
        let text = |v: &Option<String>| v.as_deref().unwrap_or("").to_lowercase();
        let order = match by {
            "line1" => text(&a.address.address.line1).cmp(&text(&b.address.address.line1)),
            "kind" => {
                let keys = |k: &[crate::AddressKind]| {
                    k.iter().map(|k| k.key()).collect::<Vec<_>>().join(",")
                };
                keys(&x.kinds).cmp(&keys(&y.kinds))
            }
            "is_primary" => x.is_primary.cmp(&y.is_primary),
            "priority" => x.priority.cmp(&y.priority),
            "valid_from" => x
                .dates
                .from()
                .map(|d| d.as_str())
                .cmp(&y.dates.from().map(|d| d.as_str())),
            "valid_to" => x
                .dates
                .to()
                .map(|d| d.as_str())
                .cmp(&y.dates.to().map(|d| d.as_str())),
            _ => text(&a.address.address.label).cmp(&text(&b.address.address.label)),
        };
        let order = if dir == SortDir::Desc {
            order.reverse()
        } else {
            order
        };
        order.then(a.address.id.cmp(&b.address.id))
    });
}
/// Bilimbi `format_address_summary/1`: line 1, locality and country.
fn summary(address: &crate::Address) -> String {
    let a = &address.address;
    [&a.line1, &a.locality, &a.country_iso]
        .into_iter()
        .filter_map(|v| v.as_deref().filter(|v| !v.is_empty()))
        .collect::<Vec<_>>()
        .join(", ")
}
/// Kinds ticked in a form: one `kind_<key>` checkbox each.
fn posted_kinds(fields: &HashMap<String, String>) -> Vec<crate::AddressKind> {
    crate::AddressKind::ALL
        .into_iter()
        .filter(|k| fields.get(&format!("kind_{}", k.key())).map(String::as_str) == Some("true"))
        .collect()
}
/// A whole number of 0 or more that fits the column, or `None`.
fn priority(text: &str) -> Option<i16> {
    text.trim().parse::<i16>().ok().filter(|p| *p >= 0)
}
