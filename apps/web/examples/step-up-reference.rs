//! Runnable specimen of Base Step-up on an owner's facade: gives or removes a
//! role through Authz `Grants::assign_role`, acting as the browser session
//! whose cookie value is in `BILIBI_SESSION_TOKEN`, and prints the outcome:
//! `ok`, or the `step_up_required` answer every channel maps from
//! `Error::StepUpRequired`. Authz has no role-assignment page yet, so this is
//! how its step-up shows in a running system. Use a disposable database:
//!
//! ```sh
//! BILIBI_SESSION_TOKEN=… cargo run -p bilibi-web --example step-up-reference -- <user id> <role code> assign|remove
//! ```
use bilibi_base::{authz::grants::Grants, step_up};
use bilibi_database::{pool, Error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [user, role, action] = args.as_slice() else {
        eprintln!("usage: step-up-reference <user id> <role code> assign|remove");
        std::process::exit(2);
    };
    let assigned = match action.as_str() {
        "assign" => true,
        "remove" => false,
        _ => return Err("the action is assign or remove".into()),
    };
    let user: i64 = user.parse()?;
    let db = pool(&std::env::var("DATABASE_URL")?)?;
    let app = bilibi_web::App::new(db.clone(), "http://localhost:3000")?;
    let scope = app
        .security
        .guard(&std::env::var("BILIBI_SESSION_TOKEN")?)
        .await?;
    let role: i64 = db
        .get()
        .await?
        .query_one(
            "SELECT id FROM roles WHERE tenant_id=$1 AND code=$2",
            &[&scope.tenant(), role],
        )
        .await?
        .get(0);
    let grants = app.services.get::<Grants>("base/authz.grants")?;
    match grants.assign_role(&scope, user, role, assigned).await {
        Ok(()) => println!("ok"),
        Err(Error::StepUpRequired { operation, reason }) => {
            println!("{}", step_up::refusal(operation, reason))
        }
        Err(error) => println!("error: {error}"),
    }
    Ok(())
}
