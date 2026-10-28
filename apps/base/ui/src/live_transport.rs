//! Shared limits for Bilibi's native live adapter; no Bilimbi module counterpart.
//! Phoenix/LiveView owns that transport; Bilibi's Web host and UI share these
//! limits, keeping JSON form events independent of the binary upload chunk size.

/// Maximum UTF-8 bytes in the complete encoded JSON intent, including its
/// request ID. One MiB carries multiple 10,000-character fields even with JSON
/// escaping (up to six bytes per UTF-16 code unit), while bounding each event.
pub const LIVE_EVENT_LIMIT: usize = 1024 * 1024;

/// The same refusal is served to the browser and returned by the host. A
/// refusal never clears the page, resets a form or enters a module handler.
pub const LIVE_EVENT_TOO_LARGE: &str = "This form is too large to send (maximum 1 MiB). Your draft is kept. Shorten the text and try again. Use a file upload for larger content.";
