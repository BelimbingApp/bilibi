//! Personal-contact embed for company/affiliation/account record hosts. The
//! host passes the Person's ID after resolving its own relation, never SQL.
use super::*;
use bilibi_ui::lists::RecordLink;

#[derive(Template)]
#[template(
    source = r#"<div id="{{ id }}" data-target="{{ id }}">{{ link }}{{ contacts }}</div>"#,
    ext = "html"
)]
struct View<'a> {
    id: &'a str,
    link: Html,
    contacts: Html,
}
pub(super) struct Contact {
    detail: detail::Detail,
    anchor: String,
    framed: bool,
}
impl Contact {
    pub fn new(pages: Pages, id: PersonId, anchor: String, framed: bool) -> Self {
        Self {
            detail: detail::Detail::new(pages, id, framed),
            anchor,
            framed,
        }
    }
}
impl LiveState<Scope, Error> for Contact {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                if event(value)? != "save_field"
                    || !["email", "phone"].contains(&value["field"].as_str().unwrap_or(""))
                {
                    return Err(Error::Invalid);
                }
                self.detail
                    .handle(request.scope, request.credential, value)
                    .await?;
            }
            let person = self
                .detail
                .pages
                .persons
                .read(request.scope, self.detail.id)
                .await?;
            let link = render(RecordLink {
                workspace: self.framed,
                kind: "core/person",
                record_id: &person.id.get().to_string(),
                href: &format!("/contacts/{}", person.id.get()),
                class: "text-link hover:underline",
                body: Html::text(&person.name),
            })?;
            Ok(LiveBody {
                html: render(View {
                    id: &self.anchor,
                    link,
                    contacts: self
                        .detail
                        .contacts(request.scope, &person, &self.anchor, false)
                        .await?,
                })?
                .to_string(),
                notice: self.detail.notice.clone(),
                rows: 1,
                ..LiveBody::default()
            })
        })
    }
}
