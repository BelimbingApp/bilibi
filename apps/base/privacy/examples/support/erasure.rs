//! Erasure approval for the walkthroughs, the way an operator and an approver
//! reach it: publish the case's default rules once per company, send a
//! verified erasure for approval, then decide it through the case's action
//! (`Engine::execute_action`), as Waiting for my approval does.
use bilibi_database::MutationBoundary;
use bilibi_privacy::{Privacy, RequestView, ERASURE_CASE, ERASURE_DECIDE};
use bilibi_tenancy::Scope;
use bilibi_workflow::{ActionRequest, ApprovalPolicyChange, Engine};
use serde_json::json;
use tokio_postgres::Client;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Publishes the erasure case's default rules for `company`, as `operator`.
pub async fn publish_rules(
    app: &bilibi_web::App,
    client: &mut Client,
    operator: &Scope,
    company: i64,
) -> Result<i64> {
    let engine = app.services.get::<Engine>(bilibi_workflow::SERVICE)?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")?;
    let expected = match engine
        .approval_policy(operator, ERASURE_CASE, company)
        .await
    {
        Ok((id, _)) => Some(id),
        Err(bilibi_database::Error::NotFound) => None,
        Err(error) => return Err(error.into()),
    };
    let mut tx = boundary
        .begin(
            client,
            operator.mutation_context("walkthrough.erasure.rules"),
        )
        .await?;
    let id = engine
        .publish_approval_policy_in(
            &mut tx,
            operator,
            ApprovalPolicyChange {
                case: ERASURE_CASE,
                company,
                expected,
                policy: bilibi_privacy::erasure_rules(),
                reason: "Erasure approval rules for the walkthrough",
            },
        )
        .await?;
    tx.commit().await?;
    Ok(id)
}

/// Sends `request` for approval as `sender`, then decides it as `approver`
/// with a reason. `approve` false rejects it.
pub async fn decide(
    app: &bilibi_web::App,
    privacy: &Privacy,
    client: &mut Client,
    sender: &Scope,
    approver: &Scope,
    request: &RequestView,
    approve: bool,
) -> std::result::Result<RequestView, bilibi_database::Error> {
    let sent = privacy
        .submit_erasure(client, sender, request.id, request.version)
        .await?;
    vote(app, approver, &sent, approve).await?;
    privacy.request(&*client, approver, request.id).await
}

/// One vote on `request`'s open round, as `approver`.
pub async fn vote(
    app: &bilibi_web::App,
    approver: &Scope,
    request: &RequestView,
    approve: bool,
) -> std::result::Result<(), bilibi_database::Error> {
    let engine = app
        .services
        .get::<Engine>(bilibi_workflow::SERVICE)
        .map_err(|_| bilibi_database::Error::Internal)?;
    let round = request
        .approval_request
        .ok_or(bilibi_database::Error::Conflict)?;
    let item = engine.approval_detail(approver, round).await?;
    let stage = item
        .request
        .stages
        .get(item.request.stage_index)
        .ok_or(bilibi_database::Error::Invalid)?;
    let seat = stage.seats.first().ok_or(bilibi_database::Error::Invalid)?;
    let mut action = ActionRequest::new(
        ERASURE_DECIDE,
        format!(
            "walkthrough:{round}:{}:{}",
            approver.user(),
            item.request.version
        ),
        item.subject_version,
    );
    action.payload = json!({
        "request": round, "version": item.request.version, "stage": stage.key, "seat": seat.key,
        "routing_version": item.request.routing_version, "approve": approve,
        "reason": if approve { "Identity and scope checked" } else { "Scope unclear; check with the requester" },
        "input": {},
    });
    engine
        .execute_action(approver, bilibi_privacy::SUBJECT, request.id, action)
        .await
        .map(|_| ())
}

/// Recalls `request`'s open round with a reason, as `initiator`.
pub async fn recall(
    app: &bilibi_web::App,
    client: &mut Client,
    initiator: &Scope,
    request: &RequestView,
) -> std::result::Result<(), bilibi_database::Error> {
    let engine = app
        .services
        .get::<Engine>(bilibi_workflow::SERVICE)
        .map_err(|_| bilibi_database::Error::Internal)?;
    let boundary = app
        .services
        .get::<MutationBoundary>("base/database.mutations")
        .map_err(|_| bilibi_database::Error::Internal)?;
    let round = request
        .approval_request
        .ok_or(bilibi_database::Error::Conflict)?;
    // The initiator reads the round's history: the decision detail is only
    // for those who may decide it.
    let version = engine
        .approval_history(initiator, round, 0, 1)
        .await?
        .request
        .version;
    let mut tx = boundary
        .begin(
            client,
            initiator.mutation_context("walkthrough.erasure.recall"),
        )
        .await?;
    engine
        .recall_approval_in(&mut tx, initiator, round, version, "Sent too early")
        .await?;
    tx.commit().await
}
