//! The personal-data contract (ADR 05): the handler every store of a person's
//! data contributes, the record kinds whose audit evidence erasure clears, the
//! sealed grants that authorise an approved request's export and erasure, and
//! the legal-hold guard. It sits below Grid, Authz and Audit, so any Base
//! module (Audit included) can answer for its own data without a dependency
//! cycle; `base/privacy` runs the requests, holds and sweeps above it. No
//! Bilimbi counterpart.
mod grant;
mod holds;
mod store;
pub use grant::{Cause, Disclosure, Erasure, Issuer, Reveal};
pub use holds::{bind_holds, HoldSession, HoldSource, Holds};
pub use store::{
    module_key, Action, Candidate, Confirmed, Deferral, Erased, ErasedRecord, Exported, File,
    FileRef, Handler, HandlerFuture, Identifier, Kept, KeptReason, KindAction, KindCount, Located,
    PersonalData, Progress, RecordKind, Stores, Subject, ERASED, MAX_CANDIDATES, PERSONAL_DATA,
    RECORD_KINDS,
};

use bilibi_module_registry::{Consumer, Layer, Module};

inventory::submit! { Consumer::new::<PersonalData>(PERSONAL_DATA, "base/personal_data", store::validate_stores) }
inventory::submit! { Consumer::new::<RecordKind>(RECORD_KINDS, "base/personal_data", store::validate_kinds) }
inventory::submit! {
    Module {
        id: "base/personal_data", layer: Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/party_ids"],
        migrations: &[], services: &[],
    }
}
