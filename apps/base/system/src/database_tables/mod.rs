//! Database Tables (Belimbing `admin/system/database-tables`): the tables of
//! the application schema, and one table's Data, Schema and Relationships.
//! Read-only, owner-only and audited; see the crate README. The facade is
//! [`DatabaseTables`]; catalog SQL, masking rules and view models stay here.
mod catalog;
mod gate;
mod index;
mod owners;
mod sensitive;
mod show;

use crate::render;
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::Module;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveState, PageRequest},
    Badge, BadgeKind, Header, Html,
};
use std::sync::Arc;

pub(crate) const INDEX: &str = "/system/database-tables";

#[derive(Clone)]
pub(crate) struct DatabaseTables {
    gate: gate::Gate,
    rules: Arc<sensitive::Rules>,
    owners: Arc<owners::Owners>,
}
impl DatabaseTables {
    pub(crate) fn new(db: Db, modules: &[&'static Module]) -> Self {
        Self {
            gate: gate::Gate::new(db),
            rules: Arc::new(sensitive::Rules::from_inventory()),
            owners: Arc::new(owners::Owners::new(modules)),
        }
    }

    /// The first paint, after the route's step-up and the owner proof.
    pub(crate) async fn render(&self, request: &PageRequest<'_, Scope>) -> Result<Html> {
        let scope = request.scope.ok_or(Error::Unauthorized)?;
        self.gate.owner(scope).await?;
        match request.operation {
            "tables" => index::Index::new(self.clone(), request)?.body(scope).await,
            "table" => {
                show::Show::new(self.clone(), request)
                    .await?
                    .body(scope)
                    .await
            }
            _ => Err(Error::NotFound),
        }
    }

    pub(crate) async fn mount(
        &self,
        request: &PageRequest<'_, Scope>,
    ) -> Result<Box<dyn LiveState<Scope, Error>>> {
        let scope = request.scope.ok_or(Error::Unauthorized)?;
        self.gate.owner(scope).await?;
        Ok(match request.operation {
            "tables" => Box::new(index::Index::new(self.clone(), request)?),
            "table" => Box::new(show::Show::new(self.clone(), request).await?),
            _ => return Err(Error::NotFound),
        })
    }
}

/// The address of one table's page.
fn table_path(name: &str) -> String {
    let mut path = format!("{INDEX}/");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            path.push(byte as char);
        } else {
            path.push_str(&format!("%{byte:02X}"));
        }
    }
    path
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::page(id=id) %}{{ header|safe }}<div class="mt-5 space-y-4">{{ body|safe }}</div>{% endcall %}"#,
    ext = "html"
)]
struct Layout<'a> {
    id: &'a str,
    header: Html,
    body: Html,
}
fn layout(id: &str, header: Header<'_>, body: Html) -> Result<Html> {
    render(Layout {
        id,
        header: render(header)?,
        body,
    })
}

#[derive(Template)]
#[template(
    source = r#"<span class="{{ class }}"{% if !title.is_empty() %} title="{{ title }}"{% endif %}>{{ text }}</span>"#,
    ext = "html"
)]
struct Text<'a> {
    text: &'a str,
    class: &'a str,
    title: &'a str,
}
fn text(text: &str, class: &str) -> Result<Html> {
    render(Text {
        text,
        class,
        title: "",
    })
}
fn titled(text: &str, class: &str, title: &str) -> Result<Html> {
    render(Text { text, class, title })
}

#[derive(Template)]
#[template(
    source = r#"<a href="{{ href }}" class="{{ class }}"{% if !title.is_empty() %} title="{{ title }}"{% endif %}>{{ text }}</a>"#,
    ext = "html"
)]
struct Link<'a> {
    href: &'a str,
    text: &'a str,
    class: &'a str,
    title: &'a str,
}
fn link(href: &str, text: &str, title: &str) -> Result<Html> {
    render(Link {
        href,
        text,
        class: "font-mono text-xs text-link hover:underline",
        title,
    })
}

fn badge(text: &str, kind: BadgeKind) -> Result<Html> {
    render(Badge {
        kind,
        body: Html::text(text),
        ..Badge::default()
    })
}

/// Badges side by side, or a muted dash when there are none.
fn badges(items: &[(&str, BadgeKind)]) -> Result<Html> {
    if items.is_empty() {
        return text("—", "text-ink-muted");
    }
    let mut inner = Html::default();
    for (label, kind) in items {
        inner.push(&badge(label, *kind)?);
    }
    render(Wrap {
        class: "flex flex-wrap gap-1",
        inner,
    })
}

#[derive(Template)]
#[template(
    source = r#"<div class="{{ class }}">{{ inner|safe }}</div>"#,
    ext = "html"
)]
struct Wrap<'a> {
    class: &'a str,
    inner: Html,
}
fn wrap(class: &str, inner: Html) -> Result<Html> {
    render(Wrap { class, inner })
}

/// Thousands separated, as Belimbing's integer display.
fn count(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    if value < 0 {
        out.insert(0, '-');
    }
    out
}

fn plural(value: usize, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        count(value as i64),
        if value == 1 { one } else { many }
    )
}
