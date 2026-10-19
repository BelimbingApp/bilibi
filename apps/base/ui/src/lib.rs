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

mod auth;
mod components;
mod error_page;
mod feedback;
mod feedback_specimen;
pub mod flex_sample;
pub mod flex_table;
pub mod forms;
mod html;
pub mod icons;
pub mod inline;
mod inline_specimen;
mod instance;
pub mod list_columns;
pub mod list_state;
pub mod lists;
pub mod params;
mod section;
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
    ResetPasswordPage, COMPANY_ARCHIVED, CREDENTIALS_REJECTED, PASSWORD_RESET, RESET_LINK_INVALID,
    RESET_REQUESTED, SESSION_EXPIRED, WORKSPACE_UNAVAILABLE,
};
pub use components::{card_body_class, card_flat, stat_strip_columns};
pub use components::{
    ActionLink, BackLink, Badge, BadgeKind, Button, ButtonVariant, Card, Chrome, CreateButton,
    Fact, Facts, Fold, Header, Icon, IconButton, IconContext, IconKind, Page, PageVariant,
    SectionHeading, StatItem, StatKind, StatStrip, DEMOTED_ACTION_CLASS, NO_COUNT,
};
pub use error_page::{
    copy_for, debug_document, error_document, DebugFrame, DebugReport, ErrorCopy,
};
pub use feedback::{
    Alert, ConfirmDialog, ConnectionBanners, ErrorSummary, Flash, FlashGroup, FlashMap, Modal,
    ModalWidth, NoticeKind, PanelNotice, Revealed, StatusKind, SummaryItem, Tab, Tabs,
    AUTO_DISMISS_MS, NO_FLASH,
};
pub use feedback_specimen::FeedbackSpecimen;
pub use flex_table::{
    Cell, ColumnKind, ColumnType, Cost, FlexColumn, FlexRow, FlexTable, Instant, Lens, Plain,
    RowMode, Scale, Typed,
};
pub use forms::{
    choices, described_by, error_sentence, field_base_class, field_class, field_inset_class,
    field_state_class, translate_error, AddMore, Attrs, Choice, CodeInput, Combobox, Field, Form,
    InputKind, LabelPlacement, MultiSelect, RadioGroup, Restricted, RestrictedField, Reveal,
    SecretInput, StoredReveal, RESTRICTED_REASON, SECRET_MASK,
};
pub use html::Html;
pub use inline::{
    CommitOutcome, CommitStates, CommitStatus, InlineChoice, InlineEdit, InlineField,
    InlineLongText,
};
pub use inline_specimen::{inline_specimen_values, InlineSpecimen};
pub use instance::{InstanceBadge, InstanceBanner};
pub use list_columns::{BuiltinColumn, ColumnsOutcome, ListColumns};
pub use list_state::{Filter, ListSpec, ListState, PageCounts, SortDir};
pub use lists::{
    Column, EmptyState, FilterToolbar, Pagination, RecordLink, Row, SortView, Table, TableEmpty,
    ToolbarControl,
};
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

pub const CSS: &str = include_str!("../assets/app.css");
pub const JS: &str = include_str!("../assets/live.js");
/// The hook controllers of the feedback, dialog and tab components.
pub const SHELL_JS: &str = include_str!("../assets/shell.js");
pub const UI_JS: &str = include_str!("../assets/ui.js");
/// The `data-hook` browser behaviour of the shared form controls, the inline
/// editors, tiling, the flexible table, timestamp display (`DateTime`,
/// `BrowserTimeZone`) and the live file input (`FileUpload`).
pub const HOOKS_JS: &str = concat!(
    include_str!("../assets/hooks.js"),
    // Both files are semicolon-free IIFEs; without this the second calls the first.
    "\n;\n",
    include_str!("../assets/inline.js"),
    "\n;\n",
    include_str!("../assets/upload.js"),
    "\n;\n",
    include_str!("../assets/tiling.js"),
    "\n;\n",
    include_str!("../assets/flex_table.js"),
    "\n;\n",
    include_str!("../assets/datetime.js")
);
/// Bilimbi's vendored page-loading bar (MIT), shown by live.js for page-level round trips.
pub const TOPBAR_JS: &str = include_str!("../assets/vendor/topbar.js");

/// Vendored public assets. Keep this allowlist bounded: never resolve a
/// request path on disk, and never make font loading depend on authentication.
pub struct StaticAsset {
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

pub fn static_asset(name: &str) -> Option<StaticAsset> {
    let (content_type, bytes): (&str, &[u8]) = match name {
        "instrument-sans-variable.woff2" => (
            "font/woff2",
            include_bytes!("../assets/static/fonts/instrument-sans-variable.woff2"),
        ),
        "instrument-sans-variable-italic.woff2" => (
            "font/woff2",
            include_bytes!("../assets/static/fonts/instrument-sans-variable-italic.woff2"),
        ),
        "OFL.txt" => (
            "text/plain; charset=utf-8",
            include_bytes!("../assets/static/fonts/OFL.txt"),
        ),
        "ui-specimen.html" => (
            "text/html; charset=utf-8",
            include_bytes!("../assets/static/ui-specimen.html"),
        ),
        "ui-feedback-driver.js" => (
            "text/javascript",
            include_bytes!("../assets/static/ui-feedback-driver.js"),
        ),
        "logo.svg" => ("image/svg+xml", include_bytes!("../assets/static/logo.svg")),
        "favicon.svg" => (
            "image/svg+xml",
            include_bytes!("../assets/static/favicon.svg"),
        ),
        "favicon.ico" => (
            "image/x-icon",
            include_bytes!("../assets/static/favicon.ico"),
        ),
        _ => return None,
    };
    Some(StaticAsset {
        content_type,
        bytes,
    })
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
