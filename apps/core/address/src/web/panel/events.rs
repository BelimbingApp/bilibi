//! The panel's events. Each write calls the Address contract through `PageContracts`; a refusal
//! becomes the panel notice and never an `Error` the host treats as fatal.
use super::*;
use crate::AttachmentInput;
use bilibi_ui::CommitStatus;

impl Panel {
    pub(super) async fn handle(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &Value,
    ) -> Result<()> {
        let fields = |ns: &str| form(value, ns);
        let event = event(value)?;
        match event {
            "clear_notice" => self.notice = None,
            "open_attach_modal" if self.can_manage => {
                self.notice = None;
                self.available = self
                    .pages
                    .addresses
                    .available(scope, self.owner, &None)
                    .await?;
                self.attach = Some(AttachDraft::default());
            }
            "close_attach_modal" => self.attach = None,
            "attach_address" => self.attach(scope, credential, &fields("address")).await?,
            "open_create_modal" if self.can_manage => {
                self.notice = None;
                if self.countries.is_empty() {
                    self.countries = self.pages.countries().await?;
                }
                // Bilimbi starts the form in Malaysia; only where it is imported.
                let mut location = Location::default();
                if self.countries.iter().any(|c| c.value == "MY") {
                    location.country_iso = "MY".into();
                }
                self.create = Some(CreateDraft {
                    input: AddressInput::default(),
                    location: LocationDraft::new(&self.pages.addresses, location).await?,
                    kinds: Vec::new(),
                    primary: false,
                    priority: "0".into(),
                    errors: Vec::new(),
                });
            }
            "open_attach_modal" | "open_create_modal" => self.forbidden(),
            "close_create_modal" => self.create = None,
            "validate_create_address" => {
                self.take_create(&fields("address")).await?;
            }
            "save_create_address" => {
                self.take_create(&fields("address")).await?;
                self.save_create(scope, credential).await?;
            }
            "edit_address_kinds" => {
                if let Some(listed) = self.listed(value.get("id").and_then(Value::as_str)) {
                    self.editing_kinds = Some(listed.address.id);
                }
            }
            "cancel_address_kinds" => self.editing_kinds = None,
            "save_address_kinds" => {
                let posted = fields("kinds");
                let id = value.get("address_id").and_then(Value::as_str);
                if let Some(listed) = self.listed(id).cloned() {
                    let mut attachment = listed.attachment.attachment.clone();
                    attachment.kinds = posted_kinds(&posted);
                    let saved = self
                        .write_attachment(
                            scope,
                            credential,
                            event,
                            &listed,
                            attachment,
                            ("Address kinds updated.", "Failed to update address kinds."),
                        )
                        .await?;
                    if saved {
                        self.editing_kinds = None;
                    }
                }
            }
            "toggle_address_primary" => {
                if let Some(listed) = self
                    .listed(value.get("id").and_then(Value::as_str))
                    .cloned()
                {
                    let mut attachment = listed.attachment.attachment.clone();
                    attachment.is_primary = !attachment.is_primary;
                    self.write_attachment(
                        scope,
                        credential,
                        event,
                        &listed,
                        attachment,
                        (
                            "Address setting updated.",
                            "Failed to update address setting.",
                        ),
                    )
                    .await?;
                }
            }
            "save_address_priority" => {
                let id = value.get("id").and_then(Value::as_str);
                let submitted = value.get("value").and_then(Value::as_str).unwrap_or("");
                if let Some(listed) = self.listed(id).cloned() {
                    let field = format!("address-priority-{}", listed.address.id);
                    match priority(submitted) {
                        Some(p) => {
                            let mut attachment = listed.attachment.attachment.clone();
                            attachment.priority = p;
                            let saved = self
                                .write_attachment(
                                    scope,
                                    credential,
                                    event,
                                    &listed,
                                    attachment,
                                    ("Address setting updated.", "Failed to update priority."),
                                )
                                .await?;
                            self.statuses.put(
                                field,
                                if saved {
                                    CommitStatus::Saved
                                } else {
                                    CommitStatus::failure()
                                },
                            );
                        }
                        // A value that is not a whole number is refused and
                        // said so, never stored as 0.
                        None => {
                            self.statuses.put(
                                field,
                                CommitStatus::refusal(
                                    "Priority",
                                    submitted,
                                    &["must be a whole number of 0 or more"],
                                ),
                            );
                            self.notice(
                                NoticeKind::Error,
                                "Priority was not saved: enter a whole number of 0 or more.",
                            );
                        }
                    }
                }
            }
            // Unlinking confirms through the shared dialog: the request holds
            // the attachment whose consequence the dialog states.
            "request_detach" => match self.listed(value.get("id").and_then(Value::as_str)) {
                Some(listed) => {
                    self.pending = Some(PendingDetach {
                        code: match self.pages.preview(scope,credential,"request_detach",
                            owner_contract(self.owner,"core.address.company.detach","core.address.person.detach"),
                            owner_input(self.owner,serde_json::json!({"attachment_id":listed.attachment.id,"version":listed.attachment.version}))).await? {
                                Ok(code)=>code,
                                Err(error)=> {let message=self.refusal(&error,"The address was not unlinked. Try again.");self.notice(NoticeKind::Error,message);return Ok(());}
                            },
                        attachment: listed.attachment.id,
                        version: listed.attachment.version,
                        label: listed.address.address.label.clone(),
                    });
                    self.notice = None;
                }
                None => self.notice(NoticeKind::Info, "That address is no longer linked."),
            },
            "cancel_detach" => self.pending = None,
            "detach_address" => {
                if let Some(pending) = self.pending.take() {
                    match self
                        .pages
                        .commit(scope,credential,"detach_address",owner_contract(self.owner,"core.address.company.detach","core.address.person.detach"),&pending.code,
                            owner_input(self.owner,serde_json::json!({"attachment_id":pending.attachment,"version":pending.version})))
                        .await?
                    {
                        Ok(()) => self.notice(NoticeKind::Success, "Address unlinked."),
                        Err(error) if fatal(&error) => return Err(Error::Unauthorized),
                        Err(WriteError::NotFound) => {
                            self.notice(NoticeKind::Info, "That address is no longer linked.")
                        }
                        Err(WriteError::Access(Error::Forbidden)) => self.forbidden(),
                        Err(error) => {
                            let message =
                                self.refusal(&error, "The address was not unlinked. Try again.");
                            self.notice(NoticeKind::Error, message);
                        }
                    }
                }
            }
            "grid" => match self
                .columns
                .handle(&bilibi_ui::params::string_values(value))
            {
                bilibi_ui::ColumnsOutcome::Sort(key) => {
                    self.sort_dir = if key == self.sort_by && self.sort_dir == SortDir::Asc {
                        SortDir::Desc
                    } else {
                        SortDir::Asc
                    };
                    self.sort_by = key;
                }
                bilibi_ui::ColumnsOutcome::Update | bilibi_ui::ColumnsOutcome::Noop => {
                    return Ok(())
                }
            },
            _ => return Err(Error::Invalid),
        }
        self.reload(scope).await
    }

    /// One attachment change; `true` when it was stored.
    async fn write_attachment(
        &mut self,
        scope: &Scope,
        credential: &str,
        event: &str,
        listed: &AttachedAddress,
        attachment: AttachmentInput,
        messages: (&str, &str),
    ) -> Result<bool> {
        let (saved, failed) = messages;
        match self
            .pages
            .submit(scope,credential,event,owner_contract(self.owner,"core.address.company.update_attachment","core.address.person.update_attachment"),
                owner_input(self.owner,serde_json::json!({"attachment_id":listed.attachment.id,"version":listed.attachment.version,"attachment":attachment})))
            .await?
        {
            Ok(_) => {
                self.notice(NoticeKind::Success, saved);
                Ok(true)
            }
            Err(error) if fatal(&error) => Err(Error::Unauthorized),
            Err(WriteError::Access(Error::Forbidden)) => {
                self.forbidden();
                Ok(false)
            }
            Err(error) => {
                let message = self.refusal(&error, failed);
                self.notice(NoticeKind::Error, message);
                Ok(false)
            }
        }
    }
    async fn attach(
        &mut self,
        scope: &Scope,
        credential: &str,
        fields: &HashMap<String, String>,
    ) -> Result<()> {
        let Some(draft) = &mut self.attach else {
            return Ok(());
        };
        let Some(address) =
            bilibi_ui::params::positive_id(fields.get("address_id").map(String::as_str))
                .and_then(|id| AddressId::new(id).ok())
        else {
            draft.error = "Please select an address.".into();
            return Ok(());
        };
        let Some(address_version) = self
            .available
            .iter()
            .find(|a| a.id == address)
            .map(|a| a.version)
        else {
            draft.error = "That address is no longer available. Reopen the picker.".into();
            return Ok(());
        };
        let input = AttachmentInput {
            kinds: posted_kinds(fields),
            is_primary: fields.get("is_primary").map(String::as_str) == Some("true"),
            priority: fields
                .get("priority")
                .and_then(|p| priority(p))
                .unwrap_or(0),
            ..AttachmentInput::default()
        };
        match self
            .pages
            .submit(
                scope,
                credential,
                "attach_address",
                owner_contract(
                    self.owner,
                    "core.address.company.attach",
                    "core.address.person.attach",
                ),
                owner_input(
                    self.owner,
                    serde_json::json!({"address_id":address,"address_version":address_version,"attachment":input}),
                ),
            )
            .await?
        {
            Ok(_) => {
                self.attach = None;
                self.notice(NoticeKind::Success, "Address attached.");
            }
            Err(error) if fatal(&error) => return Err(Error::Unauthorized),
            Err(WriteError::Access(Error::Forbidden)) => {
                self.attach = None;
                self.forbidden();
            }
            Err(error) => {
                let message = self.refusal(&error, "Failed to attach address.");
                self.notice(NoticeKind::Error, message);
            }
        }
        Ok(())
    }
    async fn take_create(&mut self, fields: &HashMap<String, String>) -> Result<()> {
        let Some(draft) = &mut self.create else {
            return Ok(());
        };
        let get = |key: &str| fields.get(key).cloned();
        draft
            .location
            .change(&self.pages.addresses, &LocationDraft::posted(fields))
            .await?;
        draft.input = AddressInput {
            label: get("label"),
            phone: get("phone"),
            line1: get("line1"),
            line2: get("line2"),
            line3: get("line3"),
            ..AddressInput::default()
        };
        draft.location.location.apply_to(&mut draft.input);
        draft.kinds = posted_kinds(fields);
        draft.primary = fields.get("is_primary").map(String::as_str) == Some("true");
        draft.priority = get("priority").unwrap_or_default();
        draft.errors = crate::model::normalize(&draft.input)
            .err()
            .unwrap_or_default();
        if priority(&draft.priority).is_none() {
            draft.errors.push(FieldError::new(
                "priority",
                "must be a whole number of 0 or more",
            ));
        }
        Ok(())
    }
    async fn save_create(&mut self, scope: &Scope, credential: &str) -> Result<()> {
        let Some(draft) = &mut self.create else {
            return Ok(());
        };
        if !draft.errors.is_empty() {
            return Ok(());
        }
        let link = AttachmentInput {
            kinds: draft.kinds.clone(),
            is_primary: draft.primary,
            priority: priority(&draft.priority).unwrap_or(0),
            ..AttachmentInput::default()
        };
        match self
            .pages
            .submit(
                scope,
                credential,
                "save_create_address",
                owner_contract(
                    self.owner,
                    "core.address.company.create_and_attach",
                    "core.address.person.create_and_attach",
                ),
                owner_input(
                    self.owner,
                    serde_json::json!({"address":draft.input,"attachment":link}),
                ),
            )
            .await?
        {
            Ok(_) => {
                self.create = None;
                self.notice(NoticeKind::Success, "Address created and attached.");
            }
            Err(error) if fatal(&error) => return Err(Error::Unauthorized),
            Err(WriteError::Invalid(errors)) => draft.errors = errors,
            Err(WriteError::Access(Error::Forbidden)) => {
                self.create = None;
                self.forbidden();
            }
            Err(error) => {
                let message = self.refusal(&error, "Failed to create address. Try again.");
                self.notice(NoticeKind::Error, message);
            }
        }
        Ok(())
    }
}
