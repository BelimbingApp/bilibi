# bilibi-events

Events (`base/events`) let a module react to something that happened in another Domain or Extension, without either module knowing the other exists.

## What is an event?

An event is a fact a module records about its own work. Any module can publish one, whether it lives in a Domain, in Core or in an Extension. (Domain-Driven Design calls this a *domain event*, where "domain" means the business, not Bilibi's Domain layer.) Two examples: "goods were received" and "an order was sent". Other modules can react to that fact later, in their own transaction.

Here is the whole idea in one example. Supply Chain receives goods, and Finance Payables wants to know so it can match the receipt against a bill:

1. Supply Chain saves the goods receipt, and in the same transaction it records `supply_chain/receiving.goods_received`.
2. Finance Payables has said it reads that event. Supply Chain doesn't know or care.
3. After Supply Chain commits, a background job hands the event to Finance Payables.
4. Finance Payables saves its own copy of what it needs for matching.

Notice what is **not** there:

- Neither crate depends on the other.
- Neither module descriptor names the other.
- Neither reads the other's tables.

Each Domain composes and boots on its own. That is the point of events: Domains and Extensions stay apart, yet they can still work together.

You may already have questions: what if Finance isn't mounted, what if it fails, what if the same receipt arrives twice? Don't worry, we cover each of these below.

## Events or ports: which to choose?

Bilibi has two ways for one Domain or Extension to use another without depending on it. Events are one; ports (`apps/base/ports/README.md`) are the other.

**Use an event when the reaction can happen later.** The publisher records the fact and moves on. It never waits for subscribers, and a subscriber can't refuse it. A subscriber that is unmounted, failing or brand new doesn't hold the publisher back. Typical reactions are copying a receipt, opening a payable or starting a workflow.

**Use a port when you need an answer now.** The caller asks inside its own transaction and gets a reply straight away. The module behind the port may be unmounted, so the caller handles `Unavailable`. A typical case is Payables reading a purchase order line to show a three-way match.

| | Event | Port |
| --- | --- | --- |
| Timing | After the publisher commits | Now, inside the caller's transaction |
| Can the other side refuse? | No | Yes |
| Other side unmounted | Publisher carries on | Caller gets `Unavailable` |

> [!WARNING]
> Never link Domains or Extensions any other way: no Cargo dependency, no descriptor edge, no SQL on another one's tables. Inside one Domain or Extension (or within Base, or within Core), call the neighbouring module directly. Boot refuses a subscriber to events from its own Domain or Extension.

## Publishing an event

A module that records an event is called the **publisher**. Publishing takes two steps: declare the event type once, then record each event as it happens.

### Declaring the event type

The publisher depends on `base/events` and declares each event type in its own namespace. The name has the form `<module ID>.<snake_case name>`:

```rust
inventory::submit! { Contribution::new(EVENTS, "supply_chain/receiving.goods_received", provenance!(), &EventType { version: 1 }) }
```

### Recording an event

Record the event in the same `MutationTransaction` as the write it describes:

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

Notice that nothing is sent here. The call writes rows in your transaction, and they commit or roll back with your own writes:

- the event;
- one pending delivery for each mounted subscriber;
- an audit row (without the payload);
- the job that will deliver it.

If `record` refuses, your whole transaction fails.

> [!NOTE]
> Events are never broadcast in memory, and `after_commit` is not a way to deliver them. The stored rows are the source of truth, so a crash between commit and delivery loses nothing.

### The four parts of an event

Each event carries a subject, a key, a version and a payload. Here is what each one is for.

**Subject** is the record the event is about, such as `goods_receipt:42`. We deliver one subject's events in the order they were recorded. So record them while you hold the owner's lock on that record.

**Key** is your idempotency key for the event. It is unique per publisher and tenant:

- Recording the same event again answers `Recorded::Existing` and writes nothing.
- A different event under a key you already used is a `Conflict`.
- Subscribers receive the key and can use it as their own.

**Version** (`EventType::version`) is the version of the payload you record now. Raise it when the payload changes in a way older readers can't handle. Keep the older versions documented while any subscriber still reads them.

**Payload** carries references and the facts subscribers need, and never secrets. It follows the queue's JSON limits:

- the root is an object;
- it nests at most 8 levels deep;
- it is at most 64 KiB encoded.

> [!TIP]
> Document every event name, version and payload in your module's README. That documented shape is the whole contract: subscribers rely on nothing else.

## Subscribing to an event

A module that reacts to an event is called a **subscriber**. It depends on `base/events`, never on the publisher.

### Declaring what you read

Contribute a subscriber under a key in your own namespace. List every event name you read and the versions you understand:

```rust
inventory::submit! { Contribution::new(SUBSCRIBERS, "finance/payables.goods_receipts", provenance!(), &Subscriber {
    events: &[On { event: "supply_chain/receiving.goods_received", versions: &[1] }],
    handler: &GoodsReceipts,
}) }
```

### Handling an event

We call your `Subscribe::handle` with three things:

- the delivery transaction;
- the `domain-events` system scope for the event's tenant;
- the `Event`.

Write only in that transaction, and return every `Change`. Your effects, the record that the delivery is done, and their audit all commit together.

To refuse an event, return an error. Your effects roll back, and we try the delivery again later.

You might be wondering why the handler doesn't get the publisher's user. **An event is a fact, not authority**, so the publisher's actor isn't carried across.

> [!WARNING]
> A handler must never open or commit another transaction, and must never do network or file IO. If you need either, put the work in your own outbox and key it by `Event::key`.

### What boot checks

Boot refuses to start when:

- a subscriber or event type has a key outside the contributing module;
- an event name isn't `<name>/<module>.<snake_case name>` (`<name>` is the publisher's Domain or Extension, or `base` or `core`);
- a module subscribes to an event from its own Domain or Extension;
- a contributor doesn't depend on `base/events`;
- a subscriber reads from a mounted publisher that doesn't declare that event, or doesn't record a version the subscriber reads.

An unmounted publisher is not an error. Its subscribers simply receive nothing.

## How delivery works

Now that you've seen both sides, let's follow an event after the publisher commits.

Each recorded event that has subscribers enqueues one `base/events-delivery` job on `base/queue`. The job runs as the `domain-events` system principal. For each mounted subscriber still pending on the event, the job delivers that subscriber's pending events for the same subject, oldest first, up to this one. Each event gets its own transaction:

1. lock the delivery row;
2. run your handler;
3. mark the delivery done;
4. write the audit.

### What you can rely on

**At least once.** If your handler refuses, panics or loses its lease, its effects roll back. The delivery records a short failure code, such as `subscriber_conflict` or `unsupported_event_version`, and the job retries.

**In order, per subject.** A later event for the same subject waits behind a refused one. Other subscribers and other subjects carry on.

**Never twice.** Once delivered, an event is never handed to that subscriber again. Overlapping jobs take turns on the row lock, and a replay skips delivered rows.

Retries follow the queue's backoff: `min(3600, 2^attempt)` seconds between tries, up to 100 attempts.

### Why an event might not arrive

There are two common reasons. Both leave the event safely recorded, and both are fixed the same way, with a replay.

**The tenant hasn't allowed delivery yet.** Declaring the `domain-events` principal grants it nothing. A tenant administrator grants it `events.delivery.deliver` once per tenant (`Grants::set_system`). Until then, deliveries are cancelled with `actor_refused` and stay pending. A replay delivers them once the grant exists.

**The subscriber wasn't mounted.** We create deliveries only for the subscribers mounted when an event is recorded. So an event recorded while a subscriber was unmounted, or before it was ever mounted, has no delivery for it. The pending rows of an unmounted subscriber wait without holding the job up.

> [!NOTE]
> Nothing is replayed automatically. The gap shows on the Events page, and an operator or agent closes it with a replay, described next.

## Closing a gap with a replay

A **gap** is the set of events a subscriber reads but has no delivery for. It usually appears after a subscriber is remounted. A **replay** closes it in two ways:

- it creates a delivery for each missed event;
- it puts the subscriber's pending deliveries back on the queue.

Delivery then runs as usual.

We compute the gap when the page or an operation asks for it, so a remount shows its gap at once. Boot writes nothing.

### Choosing where to start

Start from an event with `ReplayFrom::Event(id)`, or from a time with `ReplayFrom::Time(instant)`. A time starts at the first event recorded at or after it. Starting earlier than you need is safe, as the next part explains.

### Why replaying is safe

A replay creates a delivery only where the subscriber has none (`ON CONFLICT DO NOTHING`). It never hands over a delivered event again. A replay with nothing to create or requeue writes nothing, not even audit, and asks for no step-up.

### Limits and records

- **Batches.** One replay looks at no more than `limit` events (1 to 1,000). `next` tells you where the following batch starts.
- **Audit.** Each replay writes one `base/events.replay` change, attributed to the operator. It names the subscriber, the first and last event, the deliveries created and the deliveries pending. Each delivery then audits as `domain-events`.
- **Step-up.** The `base.events.replay` step-up is off by default, because a replay only hands over recorded facts, and never twice. A tenant can turn it on at Administration › Authorization › Step-up 2FA.
- **Mounted subscribers only.** A replay to an unmounted subscriber is refused with `NotFound`. Its pending rows wait for the remount.

### Replaying from code

The `Events` facade offers four calls:

| Call | Answers | Needs |
| --- | --- | --- |
| `subscriber_states(client, scope)` | Each subscriber's state (see the list below) | `admin.events.view` |
| `deliveries(client, scope, from_event, pending_only, limit)` | The tenant's deliveries (1 to 500 rows), with attempts and the last failure code | `admin.events.view` |
| `replay_review(client, scope, subscriber, from, limit)` | What a replay would do, writing nothing | `admin.events.replay` |
| `replay(scope, subscriber, from, limit)` / `replay_in(tx, …)` | The replay, in its own transaction or the caller's | `admin.events.replay`, step-up `base.events.replay` |

For each subscriber, `subscriber_states` reports:

- whether it is mounted, and the events it reads;
- its last delivered event;
- its pending and failing deliveries;
- its missed events (those with no delivery at all);
- `gap_from`, where the gap starts.

### Replaying from the page

Open **Administration › System › Events** (`/system/events`, which needs `admin.events.view`). The page has no Bilimbi counterpart, and its list follows the Sessions page. Each subscriber gets one row:

- its status: Up to date, Gap or Not mounted;
- the events it reads;
- its last delivered event, and when it was delivered;
- its pending deliveries, and how many are failing;
- its missed events, and which event they start from.

A row with a gap shows a Replay button, for people with `admin.events.replay`. It asks for confirmation through `ConfirmDialog`. It then calls the `base.events.replay` contract from `gap_from` (`PageContracts::submit`, ADR 01). The owner rechecks the capability and the step-up policy. If a step-up is needed, the browser goes to the check and comes back.

### Replaying from an agent

| Key | Kind | Capability |
| --- | --- | --- |
| `base.events.subscriber.list` | Read | `admin.events.view` |
| `base.events.delivery.list` | Read (`from_event`, `pending_only`, `limit`) | `admin.events.view` |
| `base.events.replay` | Reviewed write (`subscriber`, exactly one of `from_event` or `from_time`, `limit`) | `admin.events.replay`, step-up `base.events.replay` |

The guide `base.events` walks an agent through finding a gap and closing it.

## Personal data

**A publisher may declare where an event type's payload names the person and which fields are personal; any other match is only a candidate for the operator to confirm.** Store `base/events.payloads` under [`base/personal_data`](../personal_data/README.md), [ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md).

```rust,ignore
inventory::submit! { Contribution::new(PERSONAL_EVENTS, "supply_chain/receiving.goods_received", provenance!(),
    &PersonalEvent { person: Some("/received_by_person"), fields: &["/received_by_name"] }) }
```

- Linked by the person pointer: found, exported and erased field by field (`[erased]`).
- Found only by an email, or a phone's digits however formatted: listed as a candidate; erased only when confirmed. A confirmed event of an undeclared type has its whole payload replaced, once every subscriber has it (until then the store retries).
- Subscribers keep their own copies and answer for them through their own stores.

## Tables

This module owns `base_events` and `base_events_deliveries`.

## Seeing it work

The example `apps/web/examples/events-reference.rs` runs everything above on a fresh disposable database. Its header lists four compositions to run in order. It uses two reference Domains: the publisher `fixtures/containers/event_publisher` and the subscriber `fixtures/containers/event_subscriber`. They are never mounted in the Platform and never name each other.

Here is what you'll see:

- **Nothing mounted:** the publisher's command is unknown.
- **Publisher alone:** an event is recorded, with no deliveries.
- **Both mounted:** the subscriber is new, so the earlier event isn't delivered to it.
- **An unknown supplier:** a receipt from a supplier the subscriber hasn't onboarded is refused and retried, and the return of that receipt waits behind it. Once the supplier exists, both are delivered in order.
- **The same receipt twice:** one event is recorded.
- **Replay:** a replay from the first event delivers the earlier receipt, and a second replay finds nothing pending.
- **Subscriber alone:** it boots, and nothing new is recorded.

Every composition also shows boot refusing a subscriber in the same Domain as the publisher and a malformed event name.

The remount case also runs on the real Domains:

1. Mount `bilibi-dom-supply-chain` and `bilibi-dom-finance`.
2. Send an order while Finance is unmounted.
3. Remount Finance. The Events page shows `finance/payables.purchase_orders` with a gap.
4. Replay it. A bill naming that order line is now accepted and posts.

## Where to go next

- **Asking another Domain for an answer now:** see ports in `apps/base/ports/README.md`.
- **Writes, transactions and `Change`:** see `apps/base/database/README.md` and `apps/base/audit/README.md`.
- **Jobs, leases and backoff:** see `apps/base/queue/README.md`.
- **Page contracts and step-up:** see `docs/architecture/decisions/01-agent-first-operations.md`.
