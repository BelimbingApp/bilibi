# bilibi-ports

Ports (`base/ports`) let a module ask another Domain or Extension a question and get the answer straight away, without either module depending on the other.

## What is a port?

A port is a named question one module answers for others. Two examples: "what is on this purchase order line?" and "who is this employee?". Any module can ask it by name. If no mounted module answers that name, the caller is told so and carries on without it.

Here is the whole idea in one example. Finance Payables wants to show a three-way match, so it needs a purchase order line from Supply Chain:

1. Supply Chain answers a port named `supply_chain/purchasing.order_line`. It says what a request looks like and what the answer looks like.
2. Finance Payables asks that port by name, inside its own transaction, and gets the order line back.
3. If Supply Chain isn't mounted, Finance Payables gets `Unavailable` and shows "matching unavailable" instead.

Notice what is **not** there:

- Neither crate depends on the other.
- Neither module descriptor names the other.
- Neither reads the other's tables.

Each Domain composes and boots on its own. Finance builds and runs without Supply Chain; matching is then simply unavailable.

This module owns no tables. It only keeps the list of which mounted module answers which port.

> [!NOTE]
> Rust cannot call a facade it finds at run time, so the provider registers a call that takes and returns JSON instead.

You may already have questions: who checks permissions, what does the answer see of my unsaved work, what if the two sides disagree on the shape? Don't worry, we cover each of these below.

## Ports or events: which to choose?

Bilibi has two ways for one Domain or Extension to use another without depending on it. Ports are one; events (`apps/base/events/README.md`) are the other.

**Use a port when you need an answer now.** The caller asks inside its own transaction and gets a reply straight away. The module behind the port may be unmounted, so the caller must handle `Unavailable`. A typical case is Payables reading a purchase order line to show a three-way match.

**Use an event when the reaction can happen later.** The publisher records a fact, commits, and moves on. It never waits for the reaction, and the subscriber keeps its own copy. A typical case is Payables copying a goods receipt after Supply Chain saves it.

| | Port | Event |
| --- | --- | --- |
| Timing | Now, inside the caller's transaction | After the publisher commits |
| Can the other side refuse? | Yes | No |
| Other side unmounted | Caller gets `Unavailable` | Publisher carries on |

> [!WARNING]
> Never link Domains or Extensions any other way: no Cargo dependency, no descriptor edge, no SQL on another one's tables.

## Answering a port

A module that answers a port is called the **provider**. Answering takes two steps: write the call, then register it under a name.

### Naming and declaring

Each port is one call, with its own ID in the provider's namespace. The ID has the form `<module ID>.<snake_case name>`, for example `supply_chain/purchasing.order_line`. The provider also lists `base/ports` among its dependencies, so the module graph shows which modules answer ports.

**Boot refuses a port that breaks either rule:** one named outside the contributing module's namespace, or one contributed by a module that does not depend on `base/ports`.

### Writing the call

Here is a complete provider:

```rust
struct OrderLinePort;
impl PortProvider for OrderLinePort {
    fn call<'a>(&'a self, client: PortClient<'a>, scope: &'a Scope, body: Value) -> PortFuture<'a> {
        Box::pin(async move {
            let OrderLineRequest { company_id, line_id } = request(body)?;
            let line = on_client!(client, |c| order_line(c, scope, company_id, line_id).await)?;
            answer(&line)
        })
    }
}
inventory::submit! { Contribution::new(PORTS, "supply_chain/purchasing.order_line", provenance!(), &Port { provider: &OrderLinePort }) }
```

Notice the three helpers it uses:

- **`request`** reads the documented request. Any other shape is answered with `Invalid`.
- **`on_client!`** unpacks the caller's client, so the provider can call its own `&impl GenericClient` facade. We need this because `GenericClient` is sealed and can't travel inside a trait object.
- **`answer`** turns the reply into JSON.

### Permissions stay with the provider

The facade checks the provider's own capability against the caller's scope, exactly as it does for any other caller. **A port is never a way around authorization.** A consumer that holds its own capability but not the provider's is refused by the provider.

### Documenting the shape

Write the request and the answer in the provider module's README. **That documented shape is the whole contract.** The consumer never sees the provider's Rust types; it reads only what the README says.

> [!TIP]
> Treat a change to the documented shape like a change to a public API. Every consumer has its own copy of the types, and none of them will update by themselves.

## Asking a port

A module that asks a port is called the **consumer**. The consumer depends on `base/ports`, **never on the provider**. It looks up the `Ports` service (`base/ports.directory`) and calls the port by ID, using its own serde types for the documented shape:

```rust
let ports = services.get::<Ports>(bilibi_ports::SERVICE)?;
match ports.call::<_, OrderLine>(&tx, scope, "supply_chain/purchasing.order_line", &request).await {
    Ok(line) => ...,
    Err(PortError::Unavailable(_)) => ... // the module's own "matching unavailable" state
    Err(error) => ...,
}
```

The call passes two things through to the provider: your client and your scope.

### Your client, so the provider sees your unsaved work

The client is a pool connection, a `Transaction` or a `MutationTransaction`. The provider runs on that same client. So if you call inside a transaction, the provider sees your uncommitted writes and takes part in your locks.

### Your scope, unchanged

The sealed scope passes through as it is, with its actor, agent facet and impersonator. The provider acts for the same person you act for.

### Hiding a control before it fails

`Ports::available(id)` tells a page whether any mounted module answers a port. Use it to leave out a button or link, rather than offer one that would only answer `Unavailable`.

## When a call doesn't work

A call can fail in three ways. Only one of them means "something is wrong".

- **`PortError::Unavailable`:** no mounted module answers this port. This is not a failure. Show your module's own "unavailable" state, such as "matching unavailable" or "nothing to post".
- **`PortError::Refused`:** the provider answered with its own error. That is `Forbidden` when the caller lacks the provider's capability, `NotFound` for a record this scope may not see, `Invalid` for a request it does not accept, or a database failure.
- **`PortError::Shape`:** the two sides disagree on the documented shape. One side has drifted from the provider's README, and that needs fixing in code.

## Reference

| Item | Value |
| --- | --- |
| Catalog providers contribute to | `PORTS` (`base/ports.ports`) |
| Service consumers look up | `SERVICE` (`base/ports.directory`) |
| Port ID | `<module ID>.<name>`; the name starts with a lowercase letter, uses only `a-z`, `0-9` and `_`, and is at most 64 characters |
| Clients accepted | pool connection, `Transaction`, `MutationTransaction` |
| Errors | `Unavailable`, `Refused`, `Shape` |
| Tables owned | none |

## Seeing it work

`apps/web/examples/ports-reference.rs` is a consumer of the reference provider `fixtures/containers/port_reference`. That provider is a reference Domain that is never mounted in the Platform; the example's header gives the mount and unmount commands.

With nothing mounted, the example shows `Unavailable`. With the provider mounted, it shows:

- a typed answer
- the provider refusing an account that lacks its capability
- tenant isolation
- a pending write seen only through the caller's transaction
- refused request and answer shapes

## Where to go next

- **Reacting after another Domain commits:** see events in `apps/base/events/README.md`.
- **Writes, transactions and `MutationTransaction`:** see `apps/base/database/README.md`.
