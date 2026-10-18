//! `bilibi-web create-owner`: the first owner account of an install, for a
//! production database the development seed never touches. Bilimbi has no
//! command for this; it is `Bilimbi.Core.User.AdminBootstrap` run with the
//! operator's own attributes instead of the seed's. Everything is written by
//! the owners: Company provisions the operator tenant and its company, User
//! creates the account and the bootstrap receipt on the audited mutation
//! boundary, Authz grants the role, and User then gives the account its
//! Person and affiliation. The host only asks the questions.
//!
//! The password never comes from an argument, so it never lands in a shell
//! history or a process list: it is typed at the terminal with echo off, or
//! read from the first line of standard input with `--password-stdin`.
use bilibi_authz::grants::Grants;
use bilibi_core::company::{primary, NewCompany, ProvisionOutcome};
use bilibi_database::{Db, Error, Result};
use bilibi_user::{
    validate_first_administrator, BootstrapAffiliation, FieldError, FirstAdministrator,
    FirstAdministratorError, OperatorCompany,
};
use std::io::{BufRead, Write};

const USAGE: &str =
    "usage: bilibi-web create-owner [--email ADDRESS] [--name NAME] [--password-stdin]
                               [--company-name NAME --company-code CODE [--tenant-name NAME]]
Creates the first owner account of this install (the platform operator's administrator).
Asks at the terminal for anything not given. The password is typed with echo off, or is the
first line of standard input with --password-stdin (typed once, with echo off, at a terminal);
it is never an argument. Once an owner exists it creates no other; it only finishes that
owner's unfinished company link.
The company flags are needed only while no platform-operator company exists yet.";

#[derive(Default)]
struct Options {
    email: Option<String>,
    name: Option<String>,
    company_name: Option<String>,
    company_code: Option<String>,
    tenant_name: Option<String>,
    password_stdin: bool,
}

fn parse(args: &[String]) -> Option<Options> {
    let mut options = Options::default();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        if flag == "--password-stdin" {
            if options.password_stdin {
                return None;
            }
            options.password_stdin = true;
            continue;
        }
        let slot = match flag.as_str() {
            "--email" => &mut options.email,
            "--name" => &mut options.name,
            "--company-name" => &mut options.company_name,
            "--company-code" => &mut options.company_code,
            "--tenant-name" => &mut options.tenant_name,
            _ => return None,
        };
        let value = rest.next().map(|value| value.trim().to_owned())?;
        if value.is_empty() || value.starts_with("--") || slot.replace(value).is_some() {
            return None;
        }
    }
    Some(options)
}

fn usage() -> Error {
    eprintln!("{USAGE}");
    Error::Invalid
}

/// The operator company to provision when the database has none yet.
struct NewOperator {
    name: String,
    code: String,
    tenant_name: String,
}

pub async fn create_owner(db: &Db, args: &[String]) -> Result<()> {
    let options = parse(args).ok_or_else(usage)?;
    let interactive = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
    if !interactive && !options.password_stdin {
        eprintln!("No terminal to ask for the password: pass --password-stdin and write it on the first line of standard input.");
        return Err(usage());
    }
    let origin = std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into());
    let app = crate::App::new(db.clone(), &origin)?;
    let grants = app.services.get::<Grants>("base/authz.grants")?;

    // Every question first, then the validation, so nothing is provisioned
    // for an answer that is refused.
    let existing = existing_operator(db).await?;
    let new_operator = match existing {
        Some(_) => {
            if options.company_name.is_some() || options.company_code.is_some() {
                println!("The platform operator's company already exists; the company flags are not used.");
            }
            None
        }
        None => {
            let name = match &options.company_name {
                Some(name) => name.clone(),
                None => ask("Operator company name (your organisation): ", interactive)?,
            };
            let code = match &options.company_code {
                Some(code) => code.clone(),
                None => ask_or_default("Operator company code", suggest_code(&name), interactive)?,
            };
            Some(NewOperator {
                name,
                code,
                tenant_name: options.tenant_name.clone().unwrap_or_default(),
            })
        }
    };
    let mut input = FirstAdministrator {
        email: options.email.clone().unwrap_or_default(),
        name: options.name.clone().unwrap_or_default(),
        password: String::new(),
    };
    if input.email.is_empty() {
        input.email = ask("Owner email: ", interactive)?;
    }
    if input.name.is_empty() {
        let suggested = input.email.split('@').next().unwrap_or_default().to_owned();
        input.name = ask_or_default("Owner name", suggested, interactive)?;
    }
    input.password = if options.password_stdin {
        if interactive {
            hidden("Password: ")?
        } else {
            read_line()?
        }
    } else {
        ask_password()?
    };
    let errors = validate_first_administrator(&input);
    if !errors.is_empty() {
        return Err(report(&errors));
    }

    let operator = match (existing, new_operator) {
        (Some(operator), _) => operator,
        (None, Some(new)) => provision_operator(db, &new).await?,
        (None, None) => return Err(Error::Internal),
    };
    let existing_owner = match app
        .user
        .create_first_administrator(&grants, operator, &input)
        .await
    {
        Ok(created) => {
            println!(
                "Owner created: {} (account {}), Core Administrator of the platform operator{}.",
                created.email,
                created.user,
                if created.receipt_replaced {
                    "; the earlier owner's bootstrap receipt now names this account"
                } else {
                    ""
                }
            );
            None
        }
        Err(FirstAdministratorError::AccountsExist { count, first }) => Some((count, first)),
        Err(FirstAdministratorError::Validation(errors)) => return Err(report(&errors)),
        Err(FirstAdministratorError::NotOperator) => {
            eprintln!("The provisioned tenant is not the platform operator; run `bilibi-web platform-provision` and try again.");
            return Err(Error::Invalid);
        }
        Err(FirstAdministratorError::Access(error)) => return Err(error),
    };
    let settled = app.user.settle_bootstrap_affiliation().await;
    match (existing_owner, settled) {
        (None, Ok(BootstrapAffiliation::Deferred(reason))) => {
            println!(
                "The owner was not given a company yet: {reason}. Run this command again to finish it."
            );
        }
        (None, Ok(BootstrapAffiliation::Linked { .. })) => {
            println!("The owner signs in under the platform operator's company.");
        }
        (None, Ok(_)) => {}
        (None, Err(error)) => {
            eprintln!("The owner was created but could not be given a company yet. Run this command again to finish it.");
            return Err(error);
        }
        (
            Some(_),
            Ok(BootstrapAffiliation::Linked { .. } | BootstrapAffiliation::AlreadyLinked),
        ) => {
            println!("This install already had an owner whose company link was unfinished; it is finished now, and no second owner was created.");
        }
        (Some((count, first)), settled) => {
            eprintln!(
                "Not created: this install already has an owner. The platform operator holds {count} account(s); the first is {first}.\n\
                 Sign in as one of them at {origin}/login. To add people, use Users > New user there.\n\
                 If nobody can sign in any more, that is account recovery, which this command does not do."
            );
            match settled {
                Ok(BootstrapAffiliation::Deferred(reason)) => eprintln!(
                    "The owner's company link is still unfinished: {reason}. Fix that, then run this command again."
                ),
                Err(error) => {
                    eprintln!("The owner's company link could not be checked: {error}.");
                }
                Ok(_) => {}
            }
            return Err(Error::Invalid);
        }
    }
    println!("Sign in at {origin}/login as {}.", input.email);
    Ok(())
}

/// The marked operator tenant with its primary company, when both exist.
async fn existing_operator(db: &Db) -> Result<Option<OperatorCompany>> {
    let client = db.get().await?;
    let Some(tenant) = bilibi_tenancy::admin::platform_operator(&**client).await? else {
        return Ok(None);
    };
    Ok(primary::primary_company(&**client, tenant.id)
        .await?
        .map(|company| OperatorCompany {
            tenant: tenant.id,
            company: company.id.get(),
        }))
}

/// The operator tenant and company, provisioned through Company as
/// `platform-provision` does (an operator tenant without a company gets one).
async fn provision_operator(db: &Db, new: &NewOperator) -> Result<OperatorCompany> {
    let company = NewCompany {
        name: new.name.clone(),
        code: Some(new.code.clone()),
        legal_name: Some(new.name.clone()),
        ..NewCompany::default()
    };
    match primary::provision_platform_operator(db, &new.tenant_name, &company).await? {
        ProvisionOutcome::Provisioned(result) => {
            println!(
                "Platform operator ready: tenant {} “{}”, company {} “{}”.",
                result.tenant.id, result.tenant.name, result.company.id, result.company.name
            );
            Ok(OperatorCompany {
                tenant: result.tenant.id,
                company: result.company.id,
            })
        }
        ProvisionOutcome::Refused(errors) => {
            for error in errors {
                eprintln!("company {}: {}", error.field, error.message);
            }
            Err(Error::Invalid)
        }
    }
}

/// `Example Operations` → `example_operations`, the shape the company code
/// rule accepts; the operator may type another.
fn suggest_code(name: &str) -> String {
    let mut code = String::new();
    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            code.push(character);
        } else if !code.is_empty() && !code.ends_with('_') {
            code.push('_');
        }
    }
    code.trim_end_matches('_').to_owned()
}

fn report(errors: &[FieldError]) -> Error {
    for error in errors {
        eprintln!("{}: {}", error.field, error.message);
    }
    Error::Invalid
}

/// One answer from the terminal. Without a terminal a missing value is a
/// usage error, so a script never hangs on a prompt.
fn ask(prompt: &str, interactive: bool) -> Result<String> {
    if !interactive {
        eprintln!("No terminal to ask for a value: pass every flag.");
        return Err(usage());
    }
    print!("{prompt}");
    std::io::stdout().flush().map_err(|_| Error::Internal)?;
    read_line()
}

/// A value with a default: shown in the prompt and taken on an empty answer,
/// and taken without asking when there is no terminal.
fn ask_or_default(prompt: &str, default: String, interactive: bool) -> Result<String> {
    if !interactive {
        return Ok(default);
    }
    let answer = ask(&format!("{prompt} [{default}]: "), interactive)?;
    Ok(if answer.is_empty() { default } else { answer })
}

fn read_line() -> Result<String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|_| Error::Internal)?;
    Ok(line.trim_end_matches(['\n', '\r']).trim().to_owned())
}

/// The password typed twice with echo off; the policy is checked here so the
/// operator can retype at once rather than start over.
fn ask_password() -> Result<String> {
    for _ in 0..3 {
        let password = hidden("Password: ")?;
        if let Err(violation) = bilibi_user::password::check_policy(&password) {
            eprintln!("password {violation}");
            continue;
        }
        if hidden("Confirm password: ")? == password {
            return Ok(password);
        }
        eprintln!("The two entries differ; try again.");
    }
    eprintln!("No password was set.");
    Err(Error::Invalid)
}

/// Reads one line from the terminal without echoing it, restoring the
/// terminal afterwards, also on an error.
fn hidden(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::stdout().flush().map_err(|_| Error::Internal)?;
    // SAFETY: termios on the process's own standard input, with a
    // zero-initialised struct the kernel fills in before it is read.
    let original = unsafe {
        let mut term: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(libc::STDIN_FILENO, &mut term) != 0 {
            return Err(Error::Internal);
        }
        let original = term;
        term.c_lflag &= !libc::ECHO;
        if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term) != 0 {
            return Err(Error::Internal);
        }
        original
    };
    let line = read_line();
    // SAFETY: restores the settings read above on the same descriptor.
    unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &original) };
    println!();
    line
}
