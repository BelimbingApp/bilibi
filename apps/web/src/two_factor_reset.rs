//! `bilibi-web two-factor-reset --email ADDRESS`: turns off one account's
//! authenticator from the server, for an owner nobody else can reset (the
//! only admin lost their phone). There are no
//! recovery codes, so this and another admin's reset on the account page are
//! the two ways back. Everything is written by User: the audit row names the
//! system principal `two-factor-reset`, the account is signed out and
//! notified, and nothing here sees a secret or a code.
use bilibi_database::{Db, Error, Result};
use bilibi_user::{TwoFactorEmailError, TwoFactorEmailReset};

const USAGE: &str = "usage: bilibi-web two-factor-reset --email ADDRESS
Turns off the authenticator app of the account with that email, signs the account out and
notifies the person. They sign in with their password and set the app up again in Settings.
If sign-in requires 2FA, another signed-in administrator must turn that requirement off
while this account sets up a new method. Reset does not bypass the sign-in policy.
It prints nothing secret. Run it on the server, in the instance's environment.";

fn parse(args: &[String]) -> Option<&str> {
    match args {
        [flag, email] if flag == "--email" && !email.trim().is_empty() => Some(email.trim()),
        _ => None,
    }
}

pub async fn two_factor_reset(db: &Db, args: &[String]) -> Result<()> {
    let Some(email) = parse(args) else {
        eprintln!("{USAGE}");
        return Err(Error::Invalid);
    };
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into());
    let app = crate::App::new(db.clone(), &origin)?;
    match app.user.reset_two_factor_by_email(email).await {
        Ok(TwoFactorEmailReset::Reset) => println!(
            "2FA is off for {email}. The account was signed out and notified. If sign-in requires 2FA and no other method remains, ask another signed-in administrator to turn that requirement off while you set up a new method. Otherwise sign in at {origin}/login and set up the authenticator app again."
        ),
        Ok(TwoFactorEmailReset::AlreadyOff) => {
            println!("2FA is already off for {email}. Nothing changed.")
        }
        Err(TwoFactorEmailError::NotFound) => {
            eprintln!("No account uses {email}.");
            return Err(Error::NotFound);
        }
        Err(TwoFactorEmailError::Invalid) => {
            eprintln!("{email} is not a valid email address.\n{USAGE}");
            return Err(Error::Invalid);
        }
        Err(TwoFactorEmailError::Other(error)) => return Err(error),
    }
    Ok(())
}
