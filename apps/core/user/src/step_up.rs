//! The Core User operations Base Step-up may guard, each called from this
//! module's own facade. Signing in is off by default: a tenant that wants a
//! second factor at sign-in turns it on in its policy. Changing the phone
//! number codes are sent to redirects someone's second factor, so it is on.
//! Replacing text messages with an app replaces the factor itself, so it is
//! on the floor: the holder proves the phone first, whatever the policy.
use bilibi_step_up::{declare, Operation};

/// Checked in `UserService::login` on the new, still pending session.
pub const SIGN_IN: &str = "core.user.sign_in";
/// Checked in `UserService::update_account` for the phone number fact.
pub const PHONE_NUMBER: &str = "core.user.phone_number.set";
/// Checked when an account whose codes come by text message turns an
/// authenticator app on (`begin_two_factor`, `confirm_two_factor`).
pub const REPLACE_FACTOR: &str = "core.user.two_factor.replace";

declare!(
    "core/user",
    Operation::new(SIGN_IN, "Sign in").off_by_default()
);
declare!(
    "core/user",
    Operation::new(PHONE_NUMBER, "Change a user's phone number for 2FA")
);
declare!(
    "core/user",
    Operation::new(
        REPLACE_FACTOR,
        "Switch 2FA from text messages to an authenticator app"
    )
    .floor()
);
