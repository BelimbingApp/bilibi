//! The Core User operations Base Step-up may guard, each called from this
//! module's own facade. Signing in is off by default: a tenant that wants a
//! second factor at sign-in turns it on in its policy. Changing the phone
//! number codes are sent to redirects someone's second factor, so it is on.
use bilibi_step_up::{declare, Operation};

/// Checked in `UserService::login` on the new, still pending session.
pub const SIGN_IN: &str = "core.user.sign_in";
/// Checked in `UserService::update_account` for the phone number fact.
pub const PHONE_NUMBER: &str = "core.user.phone_number.set";

declare!(
    "core/user",
    Operation::new(SIGN_IN, "Sign in").off_by_default()
);
declare!(
    "core/user",
    Operation::new(PHONE_NUMBER, "Change a user's phone number for 2FA")
);
