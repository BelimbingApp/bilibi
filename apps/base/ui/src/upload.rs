//! Live file uploads: the counterpart of Bilimbi's `allow_upload` and
//! `live_file_input`, over the live socket.
//!
//! A page or panel that accepts files keeps an [`Uploads`] in its mounted
//! state, declared once at mount with one [`UploadSpec`] per file input
//! (Bilimbi's `allow_upload(:statement, accept:, max_entries:,
//! max_file_size:)`), and hands it to the host through
//! `LiveState::uploads`. The host (`apps/web/src/live.rs`) is the only
//! writer: it registers the entries the browser announces, refusing each one
//! that is too large or not accepted and the surplus over `max_entries`,
//! appends the chunks the browser streams and drops an entry the person
//! removes. The owner only reads: it renders the entries and errors through
//! the `forms::file_input` macro (an [`FileInput`] built by
//! [`Uploads::input`]) and, in the page event that acts on the files
//! (Bilimbi's `consume_uploaded_entries`), takes the completed bytes with
//! [`Uploads::consume`]. Bytes never leave this buffer any other way, and
//! they go with the mounted state when it is dropped.
//!
//! A resumable spec ([`UploadSpec::resumable`], issue 141's second phase)
//! has no Bilimbi counterpart: while the installation has a staging disk, its
//! chunks wait in Media's private staging file instead of this buffer, so a
//! dropped connection or a restarted server carries on where it stopped. The
//! browser announces the file's SHA-256 with it; the host stages the bytes,
//! and hands them to the entry here only once they hash to it, so the owner
//! consumes a resumable file exactly as any other.
//!
//! Bounds: every size is clamped by [`MAX_FILE_SIZE`] (a staged resumable
//! entry by [`MAX_RESUMABLE_FILE_SIZE`] and the largest file Media stages) and
//! [`MAX_ENTRIES`], the host accepts at most [`MAX_UPLOADS`] specs per state,
//! and a chunk must continue exactly where the last one ended. A frame is
//! `u32 id, u32 offset, bytes` with `bytes` at most [`CHUNK_SIZE`].
use askama::Template;
use serde::{Deserialize, Serialize};

/// The largest file a spec may accept, however large it asks for.
pub const MAX_FILE_SIZE: u64 = 64 * 1024 * 1024;
/// The largest file a resumable spec may accept while its bytes are staged:
/// Media's own ceiling. The installation's `media.max_bytes` lowers it, but
/// never below [`MAX_FILE_SIZE`].
pub const MAX_RESUMABLE_FILE_SIZE: u64 = 512 * 1024 * 1024;
/// The most entries a spec may hold at once.
pub const MAX_ENTRIES: usize = 20;
/// The most file inputs one mounted state may declare.
pub const MAX_UPLOADS: usize = 8;
/// The bytes of one chunk frame, after its eight-byte header (Bilimbi's
/// `chunk_size` default is 64,000).
pub const CHUNK_SIZE: usize = 64 * 1024;

/// One file input's rules (Bilimbi `allow_upload` options). `accept` holds
/// extensions (`.csv`) and media types (`text/csv`, `image/*`); empty accepts
/// anything. A spec with one entry replaces its entry on a new choice, as a
/// single `<input type=file>` replaces its selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadSpec {
    pub name: String,
    pub accept: Vec<String>,
    pub max_entries: usize,
    pub max_file_size: u64,
    /// Chunks are staged on Media's private disk and survive a dropped
    /// connection ([`UploadSpec::resumable`]).
    pub resumable: bool,
}

impl UploadSpec {
    /// A spec accepting any file, one at a time, up to `max_file_size`
    /// bytes (clamped to [`MAX_FILE_SIZE`]). The name is the file input's
    /// `name` and the key the owner consumes by: lowercase letters, digits,
    /// `_` and `-`, a module constant, so anything else panics.
    pub fn new(name: impl Into<String>, max_file_size: u64) -> Self {
        Self::declare(name.into(), max_file_size.clamp(1, MAX_FILE_SIZE), false)
    }

    /// A spec whose file survives a dropped connection: the browser
    /// announces each file's SHA-256, and the host stages the chunks on
    /// Media's private disk, keyed to the person, this page, this input, the
    /// size and that hash, so a reconnect (or the person choosing the same
    /// file again within `media.upload.staging_hours`) continues from the
    /// bytes already there. `max_file_size` is clamped to
    /// [`MAX_RESUMABLE_FILE_SIZE`], and the host lowers it to the largest
    /// file Media stages ([`UploadSpec::limit`]). Without a staging disk the
    /// spec acts as [`UploadSpec::new`]: bytes held here, at most
    /// [`MAX_FILE_SIZE`].
    pub fn resumable(name: impl Into<String>, max_file_size: u64) -> Self {
        Self::declare(
            name.into(),
            max_file_size.clamp(1, MAX_RESUMABLE_FILE_SIZE),
            true,
        )
    }

    fn declare(name: String, max_file_size: u64, resumable: bool) -> Self {
        assert!(
            !name.is_empty()
                && name.len() <= 64
                && name.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || b"_-".contains(&byte)),
            "invalid upload name {name:?}"
        );
        Self {
            name,
            accept: Vec::new(),
            max_entries: 1,
            max_file_size,
            resumable,
        }
    }

    /// The most bytes one file may have here: a staged resumable entry up to
    /// `staging`, the largest file Media stages (its `Staging::limit`:
    /// `media.max_bytes`, never below [`MAX_FILE_SIZE`]), anything else up
    /// to [`MAX_FILE_SIZE`]. Media refuses a staged file by that same
    /// figure, so it is taken as it is.
    pub fn limit(&self, staging: Option<u64>) -> u64 {
        match staging {
            Some(staged) if self.resumable => self.max_file_size.min(staged),
            _ => self.max_file_size.min(MAX_FILE_SIZE),
        }
    }

    pub fn accept<I, S>(mut self, accept: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.accept = accept
            .into_iter()
            .map(Into::into)
            .map(|value: String| value.trim().to_lowercase())
            .filter(|value| !value.is_empty())
            .collect();
        self
    }

    pub fn max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries.clamp(1, MAX_ENTRIES);
        self
    }

    /// The `accept` attribute of the input: the list joined by commas.
    pub fn accept_attr(&self) -> String {
        self.accept.join(",")
    }

    /// Whether a file of this name and media type passes `accept`: an
    /// extension entry matches the name's extension, a media type matches
    /// exactly, and `type/*` matches its family. A browser's `type` may be
    /// empty, so an extension match stands on its own.
    pub fn accepts(&self, name: &str, content_type: &str) -> bool {
        if self.accept.is_empty() {
            return true;
        }
        let name = name.to_lowercase();
        let content_type = content_type.trim().to_lowercase();
        self.accept.iter().any(|rule| {
            if let Some(extension) = rule.strip_prefix('.') {
                name.rsplit_once('.')
                    .is_some_and(|(_, actual)| actual == extension)
            } else if let Some(family) = rule.strip_suffix("/*") {
                content_type
                    .split_once('/')
                    .is_some_and(|(actual, _)| actual == family)
            } else {
                content_type == *rule
            }
        })
    }
}

/// Why an entry, or a choice of entries, was refused (Bilimbi's
/// `upload_errors` atoms, plus the transfer failure).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadError {
    TooLarge,
    NotAccepted,
    TooManyFiles,
    /// The stream broke off or the file changed while it was read.
    Interrupted,
}

impl UploadError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::NotAccepted => "not_accepted",
            Self::TooManyFiles => "too_many_files",
            Self::Interrupted => "interrupted",
        }
    }

    /// The sentence the entry list shows; Bilimbi's attachments panel copy,
    /// with the surplus message counting the spec's limit.
    pub fn message(&self, spec: &UploadSpec) -> String {
        match self {
            Self::TooLarge => "The file is larger than this page accepts.".into(),
            Self::NotAccepted => "That kind of file is not accepted.".into(),
            Self::TooManyFiles if spec.max_entries == 1 => "Choose one file at a time.".into(),
            Self::TooManyFiles => format!("Choose at most {} files.", spec.max_entries),
            Self::Interrupted => "The upload was interrupted; choose the file again.".into(),
        }
    }
}

/// A file the browser announced: what the person chose, what has arrived
/// and whether it was refused. `id` is the host's, unique per socket, and
/// the key of every later frame, cancel and DOM row.
#[derive(Debug)]
pub struct UploadEntry {
    pub id: u64,
    upload: String,
    pub name: String,
    pub size: u64,
    pub content_type: String,
    bytes: Vec<u8>,
    /// A resumable entry's SHA-256 and the bytes staged so far, while they
    /// wait on Media's disk; `None` once they are held in `bytes`.
    staged: Option<([u8; 32], u64)>,
    pub error: Option<UploadError>,
}

impl UploadEntry {
    pub fn received(&self) -> u64 {
        match self.staged {
            Some((_, received)) => received,
            None => self.bytes.len() as u64,
        }
    }

    /// Whole percent of the bytes received; an empty file is complete.
    pub fn progress(&self) -> u8 {
        (self.received() * 100)
            .checked_div(self.size)
            .map_or(100, |percent| percent.min(100) as u8)
    }

    pub fn valid(&self) -> bool {
        self.error.is_none()
    }

    /// Every byte has arrived and nothing refused the entry. A staged entry
    /// is done once the host has checked its bytes and handed them over.
    pub fn done(&self) -> bool {
        self.valid() && self.staged.is_none() && self.received() == self.size
    }
}

/// A completed file handed to the owner's event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadedFile {
    pub name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// An entry as the browser announces it, before any byte is sent.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClientEntry {
    #[serde(rename = "ref")]
    pub client_ref: String,
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub content_type: String,
}

/// A SHA-256 as the browser announces it: 64 lowercase hex characters.
fn digest(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(digest)
}

/// The host's answer per announced entry: the id to stream it under and
/// the offset to start at (a staged resumable entry's bytes already there),
/// or the error it was refused with.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Registration {
    Accepted {
        #[serde(rename = "ref")]
        client_ref: String,
        id: u64,
        chunk: usize,
        received: u64,
        done: bool,
    },
    Refused {
        #[serde(rename = "ref")]
        client_ref: String,
        error: UploadError,
    },
}

/// What a chunk did: the bytes now held and whether the file is complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkOutcome {
    pub received: u64,
    pub size: u64,
    pub done: bool,
}

/// The file inputs of one mounted state and the entries they hold.
#[derive(Debug, Default)]
pub struct Uploads {
    specs: Vec<UploadSpec>,
    entries: Vec<UploadEntry>,
    /// Upload-level refusals per spec name, cleared on the next choice.
    refusals: Vec<(String, UploadError)>,
}

impl Uploads {
    /// Declares the state's file inputs. Names are module constants, so a
    /// duplicate or more than [`MAX_UPLOADS`] panics.
    pub fn new(specs: impl IntoIterator<Item = UploadSpec>) -> Self {
        let specs: Vec<UploadSpec> = specs.into_iter().collect();
        assert!(specs.len() <= MAX_UPLOADS, "too many upload specs");
        for (index, spec) in specs.iter().enumerate() {
            assert!(
                !specs[..index].iter().any(|other| other.name == spec.name),
                "duplicate upload name {:?}",
                spec.name
            );
        }
        Self {
            specs,
            entries: Vec::new(),
            refusals: Vec::new(),
        }
    }

    pub fn spec(&self, name: &str) -> Option<&UploadSpec> {
        self.specs.iter().find(|spec| spec.name == name)
    }

    /// The entries of one input in the order they were chosen.
    pub fn entries<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a UploadEntry> + 'a {
        self.entries
            .iter()
            .filter(move |entry| entry.upload == name)
    }

    /// The input's own refusals (Bilimbi's `upload_errors(@uploads.name)`):
    /// a surplus over `max_entries`, which names no entry.
    pub fn errors(&self, name: &str) -> Vec<UploadError> {
        self.refusals
            .iter()
            .filter(|(upload, _)| upload == name)
            .map(|(_, error)| *error)
            .collect()
    }

    /// Whether any valid entry of the input is still arriving.
    pub fn in_progress(&self, name: &str) -> bool {
        self.entries(name)
            .any(|entry| entry.valid() && !entry.done())
    }

    /// Takes the completed files of one input, leaving entries still
    /// arriving or refused in place (Bilimbi `consume_uploaded_entries`).
    /// An empty result means the person has not finished choosing.
    pub fn consume(&mut self, name: &str) -> Vec<UploadedFile> {
        let mut files = Vec::new();
        let mut kept = Vec::with_capacity(self.entries.len());
        for entry in self.entries.drain(..) {
            if entry.upload == name && entry.done() {
                files.push(UploadedFile {
                    name: entry.name,
                    content_type: entry.content_type,
                    bytes: entry.bytes,
                });
            } else {
                kept.push(entry);
            }
        }
        self.entries = kept;
        self.refusals.retain(|(upload, _)| upload != name);
        files
    }

    /// Forgets every entry and refusal of one input.
    pub fn clear(&mut self, name: &str) {
        self.entries.retain(|entry| entry.upload != name);
        self.refusals.retain(|(upload, _)| upload != name);
    }

    /// The typed model the `forms::file_input` macro renders for one input,
    /// with its current entries and errors. The name is a module constant,
    /// so an undeclared one panics.
    pub fn input(&self, name: &str, id: impl Into<String>) -> FileInput<'_> {
        let spec = self
            .spec(name)
            .unwrap_or_else(|| panic!("undeclared upload {name:?}"));
        FileInput {
            id: id.into(),
            label: String::new(),
            hint: String::new(),
            class: String::new(),
            wrapper_class: String::new(),
            disabled: false,
            spec,
            entries: self
                .entries
                .iter()
                .filter(|entry| entry.upload == name)
                .collect(),
            errors: self.errors(name),
        }
    }

    /// Host side: records the files the browser chose for one input. A
    /// single-entry input replaces its entry; a multiple one adds, refusing
    /// the surplus. Each entry is checked against the spec's size and
    /// `accept`; a refused entry stays listed with its error so the person
    /// sees which file it was. A resumable input's accepted entries are then
    /// placed by the host ([`Uploads::place`]). `None` means the input is
    /// not declared.
    pub fn register(
        &mut self,
        name: &str,
        chosen: Vec<ClientEntry>,
        next_id: &mut u64,
    ) -> Option<Vec<Registration>> {
        let spec = self.spec(name)?.clone();
        self.refusals.retain(|(upload, _)| upload != name);
        if spec.max_entries == 1 {
            self.entries.retain(|entry| entry.upload != name);
        }
        let mut registrations = Vec::with_capacity(chosen.len());
        let mut count = self.entries(name).count();
        for client in chosen {
            if count >= spec.max_entries {
                self.refusals
                    .push((name.to_owned(), UploadError::TooManyFiles));
                registrations.push(Registration::Refused {
                    client_ref: client.client_ref,
                    error: UploadError::TooManyFiles,
                });
                continue;
            }
            let error = if client.size > spec.max_file_size {
                Some(UploadError::TooLarge)
            } else if !spec.accepts(&client.name, &client.content_type) {
                Some(UploadError::NotAccepted)
            } else {
                None
            };
            *next_id += 1;
            let id = *next_id;
            let entry = UploadEntry {
                id,
                upload: name.to_owned(),
                name: client.name,
                size: client.size,
                content_type: client.content_type,
                bytes: Vec::new(),
                staged: None,
                error,
            };
            registrations.push(match error {
                Some(error) => Registration::Refused {
                    client_ref: client.client_ref,
                    error,
                },
                None => Registration::Accepted {
                    client_ref: client.client_ref,
                    id,
                    chunk: CHUNK_SIZE,
                    received: 0,
                    done: entry.done(),
                },
            });
            self.entries.push(entry);
            count += 1;
        }
        Some(registrations)
    }

    pub fn contains(&self, id: u64) -> bool {
        self.entries.iter().any(|entry| entry.id == id)
    }

    /// Host side: decides where an accepted entry of a resumable input keeps
    /// its bytes. With a staging disk (`staging` is the largest file Media
    /// stages, its `Staging::limit`) and the file's SHA-256 the entry is
    /// staged, up to [`UploadSpec::limit`]; otherwise its bytes are held
    /// here, up to [`MAX_FILE_SIZE`]. Returns the refusal when the file is
    /// too large for where it can go; any other entry is left as it is.
    pub fn place(
        &mut self,
        id: u64,
        sha256: Option<&str>,
        staging: Option<u64>,
    ) -> Option<UploadError> {
        let index = self.entries.iter().position(|entry| entry.id == id)?;
        let spec = self.spec(&self.entries[index].upload)?;
        if !spec.resumable || self.entries[index].error.is_some() {
            return None;
        }
        let digest = sha256.and_then(digest).filter(|_| staging.is_some());
        let limit = spec.limit(staging.filter(|_| digest.is_some()));
        let entry = &mut self.entries[index];
        if entry.size > limit {
            entry.error = Some(UploadError::TooLarge);
            return entry.error;
        }
        entry.staged = digest.map(|digest| (digest, 0));
        None
    }

    /// Host side: the staging of a staged entry still arriving: its input,
    /// announced size and SHA-256. `None` for an entry whose bytes are held
    /// here (stream them with [`Uploads::append`]) or one refused.
    pub fn staging(&self, id: u64) -> Option<(&str, u64, [u8; 32])> {
        self.entries
            .iter()
            .find(|entry| entry.id == id && entry.error.is_none())
            .and_then(|entry| {
                entry
                    .staged
                    .map(|(sha256, _)| (entry.upload.as_str(), entry.size, sha256))
            })
    }

    /// Host side: a staged entry now has `received` bytes on Media's disk,
    /// written by a chunk or found there by a reconnect.
    pub fn staged(&mut self, id: u64, received: u64) -> Option<ChunkOutcome> {
        let entry = self.entries.iter_mut().find(|entry| entry.id == id)?;
        let (_, staged) = entry.staged.as_mut()?;
        *staged = received.min(entry.size);
        Some(ChunkOutcome {
            received: entry.received(),
            size: entry.size,
            done: false,
        })
    }

    /// Host side: a staged entry's bytes, all there and hashed as announced,
    /// now held here for [`Uploads::consume`].
    pub fn complete(&mut self, id: u64, bytes: Vec<u8>) -> Option<ChunkOutcome> {
        let entry = self.entries.iter_mut().find(|entry| entry.id == id)?;
        if entry.error.is_some() || entry.staged.is_none() || bytes.len() as u64 != entry.size {
            return None;
        }
        entry.staged = None;
        entry.bytes = bytes;
        Some(ChunkOutcome {
            received: entry.received(),
            size: entry.size,
            done: entry.done(),
        })
    }

    /// Host side: the entry's stream broke off (its staging refused a chunk
    /// or could not be written); the row says so.
    pub fn interrupt(&mut self, id: u64) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.error = Some(UploadError::Interrupted);
            entry.bytes = Vec::new();
            entry.staged = None;
        }
    }

    /// Host side: appends one chunk to an entry whose bytes are held here.
    /// The chunk must start where the bytes held end and stay within the
    /// announced size; otherwise the entry is marked interrupted and the
    /// error returned. `Err(None)` is an unknown, staged or already refused
    /// entry.
    pub fn append(
        &mut self,
        id: u64,
        offset: u64,
        bytes: &[u8],
    ) -> Result<ChunkOutcome, Option<UploadError>> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or(None)?;
        if entry.error.is_some() || entry.staged.is_some() {
            return Err(None);
        }
        if bytes.len() > CHUNK_SIZE
            || offset != entry.received()
            || entry.received() + bytes.len() as u64 > entry.size
        {
            entry.error = Some(UploadError::Interrupted);
            entry.bytes = Vec::new();
            return Err(Some(UploadError::Interrupted));
        }
        entry.bytes.extend_from_slice(bytes);
        Ok(ChunkOutcome {
            received: entry.received(),
            size: entry.size,
            done: entry.done(),
        })
    }

    /// Host side: drops one entry (Bilimbi `cancel_upload`); false when
    /// there is none. Removing an entry makes room again, so the input's
    /// surplus refusal goes with it.
    pub fn cancel(&mut self, id: u64) -> bool {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        let entry = self.entries.remove(index);
        self.refusals.retain(|(upload, _)| *upload != entry.upload);
        true
    }
}

/// A file input with its entry list (Bilimbi `live_file_input` plus the
/// entries loop its pages render). Built by [`Uploads::input`]; the
/// `forms::file_input` macro renders it, as does `Html::render(&input)`.
#[derive(Template, Debug)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% call forms::file_input(input=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct FileInput<'a> {
    pub id: String,
    pub label: String,
    pub hint: String,
    /// Replaces the control class entirely.
    pub class: String,
    pub wrapper_class: String,
    pub disabled: bool,
    pub spec: &'a UploadSpec,
    pub entries: Vec<&'a UploadEntry>,
    pub errors: Vec<UploadError>,
}

impl FileInput<'_> {
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }

    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.class = class.into();
        self
    }

    pub fn wrapper_class(mut self, class: impl Into<String>) -> Self {
        self.wrapper_class = class.into();
        self
    }

    /// A final record accepts no more files; the control and every Remove
    /// are disabled.
    pub fn disabled(mut self) -> Self {
        self.disabled = true;
        self
    }

    pub fn name(&self) -> &str {
        &self.spec.name
    }

    pub fn multiple(&self) -> bool {
        self.spec.max_entries > 1
    }

    pub fn wrapper(&self) -> &str {
        if self.wrapper_class.is_empty() {
            "mb-4"
        } else {
            &self.wrapper_class
        }
    }

    /// Bilimbi passes `class="block text-sm"` to its file inputs; the
    /// browser draws the control itself.
    pub fn control_class(&self) -> &str {
        if self.class.is_empty() {
            "block w-full text-sm text-ink file:mr-3 file:rounded-md file:border file:border-line file:bg-surface file:px-3 file:py-1.5 file:text-sm file:font-medium file:text-ink disabled:cursor-not-allowed disabled:text-ink-subtle"
        } else {
            &self.class
        }
    }

    pub fn invalid(&self) -> bool {
        !self.errors.is_empty() || self.entries.iter().any(|entry| !entry.valid())
    }

    pub fn described_by(&self) -> String {
        crate::forms::described_by(&self.id, !self.hint.is_empty(), self.errors.len())
    }

    /// The input's own refusals as sentences.
    pub fn error_messages(&self) -> Vec<String> {
        self.errors
            .iter()
            .map(|error| error.message(self.spec))
            .collect()
    }

    /// An entry's refusal as a sentence, when it has one.
    pub fn entry_message(&self, entry: &UploadEntry) -> Option<String> {
        entry.error.map(|error| error.message(self.spec))
    }
}
