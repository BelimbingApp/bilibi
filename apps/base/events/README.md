# bilibi-events

Domain events (`base/events`): how a module reacts to a fact that a module in
another container recorded, with neither naming the other. When Supply Chain
receives goods it records `supply_chain/receiving.goods_received` in its own
transaction. Finance Payables subscribes by that name and keeps its own copy
of what it needs for matching. Neither crate depends on the other, neither
descriptor names the other, and neither reads the other's tables. Each
composes and boots alone.

## Events or ports

- **Events** are the asynchronous, cross-domain seam. Use them when a
  container must react to another container's fact and the reaction can
  happen after the publisher commits: copy a receipt, open a payable, start
  a workflow. The publisher does not wait for the reaction and cannot be
  refused by it. A subscriber that is unmounted, failing or new does not
  hold the publisher back.
- **Ports** (`apps/base/ports/README.md`) are the synchronous, optional seam.
  Use one when a caller needs an answer now, inside its own transaction,
  from a module that may be unmounted: Payables reading a purchase order
  line to show a three-way match. The caller handles `Unavailable`.
- Never use a Cargo dependency, a descriptor edge or SQL on another
  container's tables. Inside one container, call the neighbouring module
  directly. Boot refuses a subscriber to its own container's events.

## Publisher

A publisher declares each event type it records in its own namespace,
`<module ID>.<snake_case name>`, and depends on `base/events`:

```rust
inventory::submit! { Contribution::new(EVENTS, "supply_chain/receiving.goods_received", provenance!(), &EventType { version: 1 }) }
```

It records the event in the same `MutationTransaction` as the write it
describes:

```rust
let events = services.get::<Events>(bilibi_events::SERVICE)?;
events.record(&mut tx, NewEvent {
    name: "supply_chain/receiving.goods_received",
    subject: &format!("goods_receipt:{id}"),
    key: &format!("received:{receipt_number}"),
    payload: json!({"receipt": receipt_number, "supplier": supplier, "quantity": quantity}),
}).await?;
tx.commit().await?;
```

- **One commit.** The event row, one pending delivery per mounted
  subscriber, the audit row (without the payload) and the delivery job
  commit or roll back with the owner's writes. A refusal poisons the
  owner's transaction. Nothing is broadcast in process, and
  `after_commit` is not a delivery path: the outbox rows are the source of
  truth.
- **Key.** The key is the publisher's idempotency key, unique per publisher
  and tenant. Recording the same event again answers `Recorded::Existing`
  and writes nothing. A different event under a used key is `Conflict`.
  Subscribers receive the key and use it as their own.
- **Subject.** The subject is the record the event is about. Delivery keeps
  one subject's events in recording order, so record them while holding
  the owner's lock on that record.
- **Version.** `EventType::version` is the version recorded now. Raise it
  when the payload changes incompatibly, and keep older versions
  documented while any subscriber reads them.
- **Payload.** The payload uses the queue's JSON bounds (object root, depth
  8, 64 KiB encoded). Carry references and the facts subscribers need,
  never secrets. Document every name, version and payload in the publisher
  module's README. That documented shape is the whole contract.

## Subscriber

A subscriber depends on `base/events`, never on the publisher, and
contributes under a key in its own namespace. It lists every event name it
reads and the versions it understands:

```rust
inventory::submit! { Contribution::new(SUBSCRIBERS, "finance/payables.goods_receipts", provenance!(), &Subscriber {
    events: &[On { event: "supply_chain/receiving.goods_received", versions: &[1] }],
    handler: &GoodsReceipts,
}) }
```

`Subscribe::handle` gets the delivery transaction, the `domain-events`
system scope of the event's tenant and the `Event`. It writes only in that
transaction and returns every `Change`. Its effects, its receipt and their
audit commit together. An error rolls the effects back and the delivery is
retried. It must never open or commit another transaction or do network or
file IO. Such work goes in the subscriber's own outbox, keyed by `Event::key`.
The publisher's actor is not carried, because an event is a fact, not
authority.

Boot refuses:

- a subscriber or event type whose key is outside the contributor's module;
- an event name that is not `<container>/<module>.<snake_case name>`;
- a subscriber to an event of its own container;
- a contributor that does not depend on `base/events`;
- a subscriber to a mounted publisher that does not declare that event, or
  that does not read the version the publisher records.

An unmounted publisher is not an error: its subscribers receive nothing.

## Delivery

Each recorded event with subscribers enqueues one `base/events-delivery` job
on `base/queue`, which runs as the `domain-events` system principal. For each
mounted subscriber still pending on the event, the job walks that
subscriber's pending events of the same subject in recording order, up to
this event. Each event is delivered in one transaction: the job locks the
delivery row, runs the subscriber, sets the receipt and writes the audit.

- **At least once.** A refusal, panic or lost lease rolls back that
  subscriber's effects. The delivery records a bounded failure code
  (`subscriber_conflict`, `unsupported_event_version`, …) and the job
  retries with the queue's backoff (`min(3600, 2^attempt)` seconds, up to
  100 attempts). A later event of the same subject waits behind the refused
  one. Other subscribers and other subjects carry on.
- **Idempotent.** A delivered row is never handed to that subscriber again.
  Overlapping jobs serialize on the row lock, and a replay skips delivered
  rows.
- **Grant.** Declaring `domain-events` grants nothing. A tenant
  administrator grants it `events.delivery.deliver` once per tenant
  (`Grants::set_system`). Without the grant, deliveries cancel with
  `actor_refused`, the rows stay pending, and a replay delivers them once
  the grant exists.
- **Mounting.** Deliveries are created for the subscribers mounted when an event is recorded. An event recorded while a subscriber is unmounted (or before it was first mounted) gets no delivery for it, and its pending rows wait without holding the job. Nothing is replayed automatically: the gap shows on the Domain Events page, and an operator or agent closes it with a replay (Operators).

## Operators

Replay closes a subscriber's gap after a remount: it creates a delivery for each event the subscriber has none for and requeues its pending ones. Delivery then runs as usual.

| Facade (`Events`) | Answers | Needs |
| --- | --- | --- |
| `subscriber_states(client, scope)` | Each subscriber: mounted, events it reads, last delivered event, pending and failing deliveries, missed events (no delivery at all) and `gap_from` | `admin.events.view` |
| `deliveries(client, scope, from_event, pending_only, limit)` | The tenant's deliveries (1–500 rows) with attempts and the last failure code | `admin.events.view` |
| `replay_review(client, scope, subscriber, from, limit)` | What a replay would do, writing nothing | `admin.events.replay` |
| `replay(scope, subscriber, from, limit)` / `replay_in(tx, …)` | The replay, in its own transaction or the caller's | `admin.events.replay`, step-up `base.events.replay` |

- **Gap.** A subscriber's missed events are those of a name it reads with no delivery for it. They are computed when the page or the operation reads them, so a remount shows its gap at once, and boot writes nothing.
- **Start.** `ReplayFrom::Event(id)` or `ReplayFrom::Time(instant)` (the first event recorded at or after it). Starting earlier is safe.
- **Idempotent.** A delivery is created only where the subscriber has none (`ON CONFLICT DO NOTHING`) and a delivered event is never handed over again. A replay with nothing to create or requeue writes nothing, not even audit, and asks for no step-up.
- **Batch.** At most `limit` events (1–1,000) are looked at; `next` names where the following batch starts.
- **Audit.** One `base/events.replay` change per replay (subscriber, first and last event, deliveries created, pending), attributed to the operator. Each delivery then audits as `domain-events`.
- **Step-up.** `base.events.replay` is off by default, because a replay hands over only recorded facts and never twice. A tenant turns it on at Administration › Authorization › Two-factor Requirements.
- **Mounted only.** Replay to an unmounted subscriber is refused (`NotFound`); its pending rows wait for the remount.

### Page

**Administration › System › Domain Events** (`/system/events`, `admin.events.view`). No Bilimbi counterpart; the list follows Sessions.

- One row per subscriber: status (Up to date, Gap, Not mounted), events it reads, last delivered event and when, pending (and failing), missed (and from which event).
- A row with a gap has Replay (`admin.events.replay`). It confirms through `ConfirmDialog`, then calls the `base.events.replay` contract from `gap_from` (`PageContracts::submit`, ADR 01). The owner rechecks the capability and the step-up policy; a step-up sends the browser to its check and back.

### Agent operations

| Key | Kind | Capability |
| --- | --- | --- |
| `base.events.subscriber.list` | Read | `admin.events.view` |
| `base.events.delivery.list` | Read (`from_event`, `pending_only`, `limit`) | `admin.events.view` |
| `base.events.replay` | Reviewed write (`subscriber`, exactly one of `from_event` or `from_time`, `limit`) | `admin.events.replay`, step-up `base.events.replay` |

The guide `base.events` tells an agent how to find a gap and close it.

This module owns `base_events` and `base_events_deliveries`.

## Proof

`apps/web/examples/events-reference.rs` drives the reference publisher
`fixtures/containers/event_publisher` and subscriber
`fixtures/containers/event_subscriber`, two Domain containers that are never
mounted in the Platform and never name each other. Its header gives the four
compositions to run in order on a fresh disposable database. The example
covers:

- With nothing mounted, the publisher's command is unknown.
- With the publisher alone, an event is recorded with no deliveries.
- With both mounted, the subscriber is new and the earlier event is not
  delivered to it.
- A receipt from a supplier the subscriber has not onboarded is refused and
  retried, while the return of that receipt waits behind it. Once the
  supplier exists, both are delivered in order.
- A receipt received twice records one event.
- A replay from the first event delivers the earlier receipt, and a second
  replay finds nothing pending.
- With the subscriber alone, it boots and nothing new is recorded.

The remount case runs on the real containers: mount `bilibi-dom-supply-chain` and `bilibi-dom-finance`, send an order with Finance unmounted, remount, and Domain Events shows `finance/payables.purchase_orders` with a gap; replay it and a bill naming that order line is accepted and posts.

Every composition also prints the boot refusals of a same-container
subscriber and of a malformed event name.
