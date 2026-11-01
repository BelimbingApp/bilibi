//! The follow channel between the tiles of one workspace: selecting a record in one tile can steer another.
//!
//! Every tile is a same-origin frame of the workspace's own document, so the
//! channel runs in the browser and never leaves the tab, and with it the
//! account: a frame can only ever talk to the workspace that framed it.
//! `assets/tiling.js` is both ends. A page opts in with two macros:
//!
//! * `record_link` of `templates/lists.html` (typed `RecordLink`) on a list
//!   row: a link alone, and inside a tile (`workspace=true`, the page is
//!   framed) a `workspace:select` button. Its click is decided in the frame
//!   when it happens: when some tile follows that kind of record, the frame
//!   announces the selection and the list stays where it is; otherwise the
//!   frame opens the record's page in the tile, as the link would. The
//!   server never receives `workspace:select`;
//! * `announce` of `templates/workspace.html` on a record's page: the record
//!   the page shows, announced once the page is in a tile.
//!
//! A fact is plain data, `{kind, id}`: `kind` is the stable module id that
//! owns the record (`core/company`), the vocabulary descriptors and menu
//! items use, and `id` fills one path segment. The workspace (Base Tiling)
//! decides which tiles follow a kind; it checks every fact with [`fact`], as
//! the hook does, since any frame could have sent it.

/// Whether `kind` is a stable module id such as `core/company`.
pub fn kind(kind: &str) -> bool {
    let mut parts = kind.split('/');
    let word = |part: Option<&str>| {
        part.is_some_and(|part| {
            part.starts_with(|c: char| c.is_ascii_lowercase())
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
    };
    word(parts.next()) && word(parts.next()) && parts.next().is_none()
}

/// Whether `id` can name a record in one path segment: at most 64 bytes, and
/// no `/`, `?`, `#`, `.`-only segment or whitespace.
pub fn record_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && ![".", ".."].contains(&id)
        && !id
            .chars()
            .any(|c| matches!(c, '/' | '?' | '#' | '\\') || c.is_whitespace() || c.is_control())
}

/// Whether `{kind, id}` is a fact a workspace may act on.
pub fn fact(kind_: &str, id: &str) -> bool {
    kind(kind_) && record_id(id)
}
