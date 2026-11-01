//! Apply both before and after owner normalization.
use bilibi_database::{Error, Result};
use serde_json::Value;

pub const MAX_DEPTH: usize = 8;
pub const MAX_MEMBERS: usize = 100;
pub const MAX_KEY_BYTES: usize = 128;
pub const MAX_STRING_BYTES: usize = 16_384;
pub const MAX_ENCODED_BYTES: usize = 65_536;

pub fn validate(args: &Value) -> Result<()> {
    if !args.is_object() {
        return Err(Error::Invalid);
    }
    walk(args, 0)?;
    if serde_json::to_vec(args).map_err(|_| Error::Invalid)?.len() > MAX_ENCODED_BYTES {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn walk(value: &Value, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::Invalid);
    }
    match value {
        Value::String(s) if s.len() > MAX_STRING_BYTES => return Err(Error::Invalid),
        Value::Array(items) => {
            if items.len() > MAX_MEMBERS {
                return Err(Error::Invalid);
            }
            for item in items {
                walk(item, depth + 1)?;
            }
        }
        Value::Object(items) => {
            if items.len() > MAX_MEMBERS {
                return Err(Error::Invalid);
            }
            for (key, item) in items {
                if key.len() > MAX_KEY_BYTES {
                    return Err(Error::Invalid);
                }
                walk(item, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
