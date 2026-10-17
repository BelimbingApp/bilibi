# bilibi-ports

Optional ports (`base/ports`): how a module reaches a module it must not
depend on. Bilimbi's Payables finds Supply Chain's purchasing and receiving
facades by descriptor ID at run time (`OrderSource`), and Payroll finds People
the same way (`PeopleSource`); Finance builds and runs without either, and
matching or posting is then unavailable. Rust has no `apply/3`, so the
provider registers a typed-JSON call instead. This module owns no tables.

Use a port when the caller needs an answer now, inside its own transaction,
and can handle `Unavailable`. When a container reacts to another container's
fact after it commits (Payables copying a goods receipt), use a domain event
(`apps/base/events/README.md`), not a port: the publisher does not wait on
the reaction and the subscriber keeps its own copy. Never use a Cargo
dependency, a descriptor edge or SQL on another container's tables instead.

## Provider

A module contributes one `Port` per call, under an ID in its own namespace,
`<module ID>.<snake_case name>` (`supply_chain/purchasing.order_line`), and
declares `base/ports` among its dependencies. Boot refuses a port named
outside the contributor's module or contributed by a module that does not
depend on `base/ports`.

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

- `request` reads the documented request and answers `Invalid` for any other
  shape; `answer` serializes the reply.
- `on_client!` unpacks the caller's client so the provider calls its own
  `&impl GenericClient` facade (`GenericClient` is sealed and cannot cross a
  trait object).
- The facade checks the provider's own capability against the scope, exactly
  as for any other caller. A port is never a way around authorization.
- Document the request and answer in the provider module's README. That
  documented shape is the whole contract.

## Consumer

A consumer declares `base/ports` (never the provider), resolves the `Ports`
service (`base/ports.directory`) and calls the port ID with its own serde
types for the documented shape:

```rust
let ports = services.get::<Ports>(bilibi_ports::SERVICE)?;
match ports.call::<_, OrderLine>(&tx, scope, "supply_chain/purchasing.order_line", &request).await {
    Ok(line) => ...,
    Err(PortError::Unavailable(_)) => ... // the module's own "matching unavailable" state
    Err(error) => ...,
}
```

- **The caller's client.** `client` is a pool connection, a `Transaction` or
  a `MutationTransaction`. The provider runs on it, so it sees the caller's
  uncommitted writes and takes part in its locks.
- **The caller's scope.** The sealed scope passes through unchanged, with its
  actor, agent facet and impersonator.
- **Errors.** `PortError::Unavailable` means no mounted module provides the
  port. `Refused` carries the provider's own error: `Forbidden` without its
  capability, `NotFound`, `Invalid` or a database failure. `Shape` means the
  two sides disagree on the documented shape.
- **Withholding controls.** `Ports::available` lets a page withhold a control
  instead of offering one that would answer `Unavailable`.

## Proof

`apps/web/examples/ports-reference.rs` is a consumer of the reference
provider `fixtures/containers/port_reference`, a Domain container that is
never mounted in the Platform. Its header gives the mount and unmount
commands. It shows `Unavailable` with nothing mounted. Mounted, it shows a
typed answer, the provider's own refusal of an account without its
capability, tenant isolation, a pending write seen only through the caller's
transaction, and refused request and answer shapes.
