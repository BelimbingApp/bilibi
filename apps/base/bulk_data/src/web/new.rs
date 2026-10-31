//! `/imports/new`: choose what the rows are and the CSV file. Receiving the
//! file opens its run and goes on to map it.
use super::{dismiss, render, view, Pages, INDEX};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, Uploads},
    params,
    upload::UploadSpec,
    BackLink, Button, ButtonVariant, Card, Field, Form, Header, Html, InputKind, StatusKind,
};

/// The file input's name: the key the page consumes by.
const UPLOAD: &str = "csv";

pub(super) struct Upload {
    pages: Pages,
    uploads: Uploads,
    record_type: String,
    error: Option<String>,
    flash: Option<(StatusKind, String)>,
}

impl Upload {
    pub(super) fn new(pages: Pages) -> Self {
        Self {
            pages,
            uploads: Uploads::new([UploadSpec::new(UPLOAD, bilibi_ui::upload::MAX_FILE_SIZE)
                .accept([".csv", "text/csv"])]),
            record_type: String::new(),
            error: None,
            flash: None,
        }
    }

    pub(super) async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let object = pages.db.get().await?;
        let client: &tokio_postgres::Client = &object;
        let types = pages.bulk.types(client, scope).await?;
        let (max_bytes, max_rows) = pages.bulk.limits(client).await?;
        drop(object);
        let header = render(Header {
            title: Html::text("New Import"),
            subtitle: "Bring records in from a CSV file. You map its columns and check every row before anything is written.",
            actions: render(BackLink {
                href: INDEX,
                id: "import-new-back",
                title: "Back to imports",
                class: "",
            })?,
            ..Header::default()
        })?;
        let body = if types.is_empty() {
            Html::text("You cannot import any kind of record. Ask your administrator for the permission to import the records you need.")
        } else {
            let options: Vec<(String, String)> = types
                .iter()
                .map(|info| (info.label.to_owned(), info.key.to_owned()))
                .collect();
            let mut fields = render(
                Field::in_form("import", "record_type")
                    .id("import-record-type")
                    .kind(InputKind::Select)
                    .label("Records")
                    .prompt("Choose what the rows are")
                    .options(choices(&options))
                    .value(self.record_type.clone())
                    .errors(self.error.iter().cloned().collect()),
            )?;
            fields.push(&render(
                self.uploads
                    .input(UPLOAD, "import-file")
                    .label("CSV file")
                    .hint(format!(
                        "UTF-8 text with a header row; at most {} and {max_rows} rows.",
                        crate::size(max_bytes)
                    )),
            )?);
            render(Form {
                id: "import-new-form",
                event: "receive",
                change: false,
                class: "",
                body: fields,
                actions: render(Button {
                    variant: ButtonVariant::Primary,
                    id: "import-new-upload",
                    button_type: "submit",
                    working: "Uploading…",
                    body: Html::text("Upload"),
                    ..Button::default()
                })?,
            })?
        };
        let card = render(Card {
            id: "import-new-card",
            body,
            ..Card::default()
        })?;
        view("import-new", &self.flash, vec![header, card])
    }

    /// Opens the run for the chosen file; `Some(path)` to go on to it.
    async fn receive(
        &mut self,
        scope: &Scope,
        value: &serde_json::Value,
    ) -> Result<Option<String>> {
        let form = params::form_values(value, "import");
        self.record_type = form.get("record_type").cloned().unwrap_or_default();
        self.error = None;
        self.flash = None;
        let Some(file) = self.uploads.consume(UPLOAD).into_iter().next() else {
            self.flash = Some((StatusKind::Info, "Choose a CSV file first.".into()));
            return Ok(None);
        };
        if self.record_type.is_empty() {
            self.error = Some("Choose what the rows are.".into());
            return Ok(None);
        }
        match self
            .pages
            .bulk
            .upload(scope, &self.record_type, &file.name, &file.bytes)
            .await
        {
            Ok(Ok(id)) => Ok(Some(format!("/imports/{id}"))),
            Ok(Err(why)) => {
                self.flash = Some((StatusKind::Error, why));
                Ok(None)
            }
            Err(Error::NotFound) => {
                self.error = Some("Choose what the rows are.".into());
                Ok(None)
            }
            Err(Error::Forbidden) => {
                self.flash = Some((
                    StatusKind::Error,
                    "You do not have permission to import these records.".into(),
                ));
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

impl LiveState<Scope, Error> for Upload {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut redirect = None;
            match request.input {
                LiveInput::Refresh | LiveInput::Params(_) => {}
                LiveInput::Event(value) => {
                    let get = |key: &str| value.get(key).and_then(serde_json::Value::as_str);
                    match get("type").ok_or(Error::Invalid)? {
                        "receive" => redirect = self.receive(request.scope, value).await?,
                        "clear-flash" => dismiss(&mut self.flash, get("key")),
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = self.render(request.scope).await?;
            let flash = redirect.as_ref().map(|_| {
                (
                    StatusKind::Success,
                    "File received. Map its columns, then check it.".to_owned(),
                )
            });
            Ok(LiveBody {
                html,
                redirect,
                flash,
                ..LiveBody::default()
            })
        })
    }
    fn uploads(&mut self) -> Option<&mut Uploads> {
        Some(&mut self.uploads)
    }
}
