//! Shared UI: document/shell templates, the component primitives in
//! `templates/components.html` with their typed models, the feedback, dialog
//! and tab components (`feedback.rs`), the icon registry and
//! the vendored asset allowlist. Only shared fragments may use `safe`.
use askama::Template;
pub mod download;
pub mod page;

// Lets the shared macro file name this crate's items as `bilibi_ui::...` from
// inside the crate and from every importing module alike.
extern crate self as bilibi_ui;

mod assets;
pub use assets::{Assets, StaticAsset};

mod auth;
mod colour;
pub use colour::{ColourChoice, ColourInput, ColourRole};
mod components;
mod error_page;
mod feedback;
mod feedback_specimen;
pub mod flex_sample;
pub mod flex_table;
pub mod forms;
mod html;
mod icons;
pub use icons::{Icons, SvgIcon};
pub mod inline;
mod inline_specimen;
mod instance;
pub mod list_columns;
pub mod list_state;
pub mod lists;
mod page_search;
pub mod params;
mod record_fields;
pub use record_fields::RecordField;
mod section;
mod settings;
mod shell;
mod specimen;
pub mod upload;
pub mod workspace;
pub use shell::{
    navigation, navigation_unpinned, version_badges, AppShell, DisplayPreferences, Framed, NavNode,
    ShellContext, VersionBadge,
};

pub use auth::{
    AuthLayout, ForgotForm, ForgotPasswordPage, LoginForm, LoginPage, LoginWorkspace, ResetForm,
    ResetPasswordPage, SignInChallengePage, COMPANY_ARCHIVED, CREDENTIALS_REJECTED, PASSWORD_RESET,
    RESET_LINK_INVALID, RESET_REQUESTED, SESSION_EXPIRED, SIGN_IN_THROTTLED, WORKSPACE_UNAVAILABLE,
};
pub use components::{button_class, card_body_class, card_flat, stat_strip_columns};
pub use components::{
    ActionLink, BackLink, Badge, BadgeKind, Button, ButtonVariant, Card, Chrome, CreateButton,
    Fact, Facts, Fold, Header, Icon, IconButton, IconContext, IconKind, Page, PageVariant,
    SectionHeading, StatItem, StatKind, StatStrip, DEMOTED_ACTION_CLASS, NO_COUNT,
};
pub use error_page::{
    copy_for, debug_document, error_document, DebugFrame, DebugReport, ErrorCopy,
};
pub use feedback::{
    Alert, ConfirmDialog, ConnectionBanners, ErrorSummary, Flash, FlashGroup, FlashMap,
    FloatingFlashGroup, Modal, ModalWidth, NoticeKind, PanelNotice, Revealed, StatusKind,
    SummaryItem, Tab, Tabs, AUTO_DISMISS_MS, NO_FLASH,
};
pub use feedback_specimen::FeedbackSpecimen;
pub use flex_table::{
    Cell, ColumnKind, ColumnType, Cost, FlexColumn, FlexRow, FlexTable, Instant, Lens, Plain,
    RowMode, Scale, Typed,
};
pub use forms::{
    choices, described_by, error_sentence, field_base_class, field_class, field_inset_class,
    field_state_class, translate_error, AddMore, Attrs, Choice, CodeInput, Combobox, Field, Form,
    InputKind, LabelPlacement, LocationAction, LocationReport, MultiSelect, ProofInput,
    ProofMethod, RadioGroup, Restricted, RestrictedField, Reveal, SecretInput, StepUp,
    StoredReveal, RESTRICTED_REASON, SECRET_MASK,
};
pub use html::Html;
pub use inline::{
    CommitOutcome, CommitStates, CommitStatus, InlineChoice, InlineEdit, InlineField, InlineForm,
    InlineLongText,
};
pub use inline_specimen::{inline_specimen_values, InlineSpecimen};
pub use instance::{InstanceBadge, InstanceBanner, InstanceStatus};
pub use list_columns::{BuiltinColumn, ColumnsOutcome, ListColumns};
pub use list_state::{Filter, ListSpec, ListState, PageCounts, SortDir};
pub use lists::{
    Column, EmptyState, FilterToolbar, Pagination, RecordLink, Row, SortView, Table, TableEmpty,
    ToolbarControl,
};
pub use page_search::{page_search, RecordGroup, RecordHit, SearchPage, SearchRecords};
pub use settings::{SettingsTab, SETTINGS_LOCATION};
pub use specimen::ComponentsSpecimen;
pub use upload::{
    ChunkOutcome, ClientEntry, FileInput, Registration, UploadEntry, UploadError, UploadSpec,
    UploadedFile, Uploads,
};

/// The HTML document around a page. `title` follows Bilimbi's live title:
/// the page's own name, or the product name when a page has none.
#[derive(Template)]
#[template(path = "document.html")]
pub struct Document<'a> {
    pub title: &'a str,
    pub body: &'a str,
    /// The account's explicit `light`/`dark` choice, stamped on `<html>` so the
    /// first paint is themed; anything else follows the device.
    pub theme: &'a str,
}

pub struct Renderer;
impl Renderer {
    pub fn render<T: Template>(&self, template: T) -> std::result::Result<String, askama::Error> {
        template.render()
    }
}
inventory::submit! {
    bilibi_module_registry::Module {
        id:"base/ui",layer:bilibi_module_registry::Layer::Base,dependencies:&["base/module_registry"],migrations:&[],
        services:&[bilibi_module_registry::Service {id:"base/ui.renderer",factory:|_|Ok(std::sync::Arc::new(Renderer))}],
    }
}
