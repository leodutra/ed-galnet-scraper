---
name: rust-type-driven
description: >
  Idiomatic type-driven Rust for domain and backend code: parse-don't-validate at trust
  boundaries, newtypes for domain concepts, illegal states unrepresentable, typed errors
  with thiserror, capability traits, async/cancellation discipline, and invariant testing.
  Use this skill whenever writing, reviewing, refactoring, or designing Rust that models a
  domain — HTTP/RPC handlers, database or queue boundaries, state machines, value objects,
  error types, service layers. Trigger on mentions of: parse don't validate, newtype,
  illegal states, typestate, thiserror, anyhow, TryFrom, domain model, validation, Result
  or Option design, trait/DI decisions, tokio tasks, cancellation safety, spawn_blocking,
  proptest. Also trigger when Rust code under review uses raw String/i64/Uuid in signatures,
  stringly typed errors, `_ => {}` on its own enums, or unwrap()/expect() outside tests.
  For GPU/render/performance-critical Rust use rust-wgpu-functional; for Bevy ECS layout
  use rust-bevy-architecture.
---

# Rust — Idiomatic Type-Driven Patterns

## Precedence

These guidelines MUST override default Rust conventions in the project you are working on. If following them creates
an important technical conflict, raise it during planning. ADRs MAY override these patterns when explicitly recorded.
Interpret `MUST`, `MUST NOT`, `SHOULD`, `SHOULD NOT`, and `MAY` literally.

## Philosophy

Use Rust's type system, ownership model, and enums as architecture, not just syntax to enforce discipline that other languages simulate with patterns.

Two axioms drive the type-level decisions:

1. **If it compiles, it's valid.** Encode rules in types so invalid states CANNOT exist.
2. **Ownership is design.** Who owns a value, who borrows it, and who consumes it are not
   implementation details — they shape the API.

- The standard library SHOULD be preferred over new dependencies unless a crate provides clear value.
- `unsafe` MUST be minimized; every unsafe block MUST document its invariants.

---

## Parse, Don't Validate

All external data is untrusted. Every trust boundary reparses raw input into validated domain
types: HTTP/RPC, database reads, queues, files, and environment (vars/others).

Rules:

- External data MUST be parsed at the boundary and trusted afterward.
- Inbound conversions SHOULD use `TryFrom` / `TryInto`.
- Outbound conversions SHOULD use `From` / `Into`.
- Code MUST NOT re-check an invariant after successful parsing.

```rust
#[derive(Deserialize)]
pub struct CreateOrderRequest {
    pub customer_id: String,
    pub items: Vec<OrderItemDto>,
}

impl TryFrom<CreateOrderRequest> for CreateOrder {
    type Error = ValidationError;

    fn try_from(req: CreateOrderRequest) -> Result<Self, Self::Error> {
        Ok(CreateOrder {
            customer: CustomerId::parse(req.customer_id)?,
            items: req.items
                .into_iter()
                .map(OrderItem::try_from)
                .collect::<Result<Vec<_>, _>>()?,
        })
    }
}
```

---

## Type-Driven Design

### Core rules

- Illegal states MUST be unrepresentable.
- Newtypes SHOULD be used for domain concepts, invariants, and argument-order safety.
- Local implementation details MUST NOT be wrapped without a real invariant.
- Typestate SHOULD be used when the workflow has 3+ states, invalid transitions are costly, and the type crosses module boundaries.
- Otherwise, code SHOULD use an enum.
- Enums + structs SHOULD be preferred over class hierarchies.
- State transitions SHOULD default to immutable values.

### Newtypes

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Email(String);

// No pub constructor. The only way to get an Email is through parse().
// After parse() succeeds, the Email is trusted everywhere — no re-validation.
impl Email {
    pub fn parse(raw: impl Into<String>) -> Result<Self, ValidationError> {
        let value = raw.into();
        if value.contains('@') && value.len() <= 254 {
            Ok(Self(value))
        } else {
            Err(ValidationError::InvalidEmail(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
```

### Illegal states

```rust
enum Shipment {
    Pending { order_id: OrderId },
    Shipped { order_id: OrderId, tracking: TrackingNumber },
    Delivered { order_id: OrderId, tracking: TrackingNumber, delivered_at: DateTime },
    Cancelled { order_id: OrderId, reason: CancellationReason },
}
```

### Partial updates

Partial updates SHOULD be modeled as `Option` fields whose present values are already parsed domain types.

```rust
pub struct OrderUpdate {
    pub new_status: Option<OrderStatus>,
    pub shipping: Option<ShippingAddress>,
}

impl Order {
    pub fn apply(self, update: OrderUpdate) -> Result<Self, OrderError> {
        let status = match update.new_status {
            Some(next) => self.status.transition_to(next)?,
            None => self.status,
        };

        Ok(Self { status, ..self })
    }
}
```

---

## Error Modeling

Errors are typed and structured, never stringly typed.

```rust
#[derive(Debug, thiserror::Error)]
pub enum OrderError {
    #[error("cannot transition from {from} to {to}")]
    InvalidTransition { from: OrderStatus, to: OrderStatus },
    #[error("order total exceeds credit limit: {total} > {limit}")]
    CreditLimitExceeded { total: Money, limit: Money },
    #[error("empty order")]
    EmptyOrder,
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Order(#[from] OrderError),
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error("database query failed")]
    Database(#[from] sqlx::Error),
}
```

Rules:

- Error variants MUST carry typed context, not ad-hoc strings.
- Errors SHOULD compose through explicit `From` impls so `?` stays honest.
- Libraries SHOULD use `thiserror`; `anyhow` / `eyre` SHOULD be limited to process boundaries.
- Application code using `anyhow` SHOULD add `.context(...)` when propagating fallible operations.
- Code MUST NOT use `Err("something went wrong".into())`.

---

## Invariants and Assertions

Enforce invariants in this order:

1. Encode the invariant in a type.
2. Return a typed error.
3. Use `debug_assert!()` for impossible states that indicate a bug.
4. Use `assert!()` only when continuing would be unsafe or corrupt state.

Rules:

- User input MUST NOT be validated with assertions.
- Exhaustive `match` SHOULD be preferred over `unreachable!()`.
- If an invariant can live in the type system, it MUST NOT be enforced at runtime.

```rust
impl Order {
    pub fn confirm(self) -> Result<ConfirmedOrder, OrderError> {
        if self.total > self.credit_limit {
            return Err(OrderError::CreditLimitExceeded {
                total: self.total,
                limit: self.credit_limit,
            });
        }

        debug_assert!(!self.items.is_empty(), "Order invariant violated: empty items");
        Ok(ConfirmedOrder { /* ... */ })
    }
}
```

---

## Behavior Rules

Functions, APIs, and state transitions follow these design principles.

- Code SHOULD tell, not ask.
- Functions SHOULD be total. Code MUST NOT panic on valid input.
- Code MUST match exhaustively on its own enums.
- `Result` / `Option` combinators SHOULD be used when they clarify the flow; pipelines SHOULD NOT be forced where straight-line code is clearer.

### Function design

- Functions SHOULD keep one level of abstraction.
- Functions SHOULD be small and single-purpose.
- Return values SHOULD be preferred over incidental side effects.
- Functions SHOULD borrow inputs when ownership is not required.
- Constructors SHOULD accept owned-friendly inputs such as `impl Into<String>` and return owned values.
- Important return values SHOULD use `#[must_use]` when ignoring them is likely a bug.
- Comments SHOULD explain why, not what, and SHOULD appear only when necessary.

### Mutation discipline

- APIs SHOULD default to immutable interfaces.
- `&mut self` SHOULD be used when it is the natural model, improves performance, or avoids unnecessary allocation.
- Interior mutability (`Cell`, `RefCell`, `Mutex`) in pure, side-effect-free domain logic MUST be justified by multithreading or structural dependency needs.

### Allocation discipline

- Owned types SHOULD be the default.
- Borrowed types SHOULD be used for transient parsing and short-lived views.
- Struct lifetimes SHOULD NOT be introduced unless profiling shows a measurable need.
- Lifetimes SHOULD be elided when the compiler can infer them.
- Borrows SHOULD be scoped narrowly to release them before unrelated work.
- Code SHOULD prefer borrowing over cloning when ownership does not need to change.

### Naming

- Types MUST be named by meaning, not structure.
- Functions MUST be named by what they do, not how they do it.
- Generic role names like `Service`, `Manager`, `Helper`, `Utils`, and `Misc` MUST NOT be introduced.
- Domain vocabulary SHOULD be used.

---

## Dependency Injection

- Traits SHOULD define capabilities: `LoadOrders`, `ChargePayment`, `PublishEvent`.
- Callers SHOULD depend on capabilities; adapters SHOULD implement them at the edges.
- A trait SHOULD NOT be introduced for a single implementation unless a real second implementation or test double is needed.
- Async trait methods MAY be used, but they are NOT dyn-compatible; prefer generics unless trait objects are required.

### When to introduce a trait

| Situation                                          | Use a trait? |
|----------------------------------------------------|--------------|
| 2+ real implementations                            | Yes          |
| 1 real impl but need test doubles for side effects | Yes          |
| Pure function with no side effects                 | No           |
| Single impl, easily testable directly              | No           |

---

## Async / Runtime Rules

### Task boundaries

- Values moved into spawned tasks MUST satisfy runtime bounds such as `Send + 'static`.
- Code MUST move owned data into tasks and MUST NOT borrow across task boundaries.
- `Arc<T>` + immutable state SHOULD be preferred. Shared mutation MUST be explicitly justified.
- `std::sync::MutexGuard` MUST NOT be held across `.await`.
- `tokio::sync::Mutex` SHOULD be used only when a lock truly must span `.await`.

### Cancellation

- Every `await` MUST be treated as a cancellation point.
- Workflows with money, inventory, or external side effects MUST be idempotent or transactionally safe.

### Blocking work

- Async code MUST NOT block the runtime.
- Blocking I/O or CPU-heavy work MUST use async-aware APIs or `spawn_blocking`.

### Panic policy

- `panic!`, `unwrap()`, and `expect()` MUST NOT appear in production paths.
- They MAY be used in tests and unrecoverable bootstrap code in `main.rs` with a clear message.
- Library code MUST return typed errors.

---

## Testing Strategy

- Unit tests for pure domain logic SHOULD be the default: fast, deterministic, no mocks.
- Integration tests SHOULD live in `tests/` and exercise the public API only.
- Parse tests SHOULD cover accepted and rejected inputs.
- Property tests SHOULD be used for value objects and parsers with clear invariants.
- Cancellation tests SHOULD be added for async workflows with externally visible side effects.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_order_can_transition_to_confirmed() {
        let order = Order::pending(customer_id(), items());
        let confirmed = order.confirm().unwrap();
        assert!(matches!(confirmed.status(), OrderStatus::Confirmed));
    }
}
```

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn money_from_cents_roundtrips(cents in 0i64..=i64::MAX) {
        let money = Money::from_cents(cents);
        prop_assert_eq!(money.to_cents(), cents);
    }
}
```

### What not to test

- Private helpers SHOULD be tested through the public API.
- Framework glue SHOULD NOT be tested unless custom logic is involved.
- Tests SHOULD NOT target properties the compiler already guarantees.

### Type invariant checklist

- Constructor invariants MUST be covered.
- Transition invariants MUST be covered.
- Roundtrip invariants MUST be covered.
- Time and concurrency invariants MUST be covered where relevant.

---

## Anti-Patterns to Reject

- Code MUST NOT use raw `String`, `i64`, or `Uuid` in meaningful signatures.
- Code MUST NOT re-validate a parsed type.
- Code MUST NOT use stringly typed errors.
- Code MUST NOT use `_ => {}` on its own enums.
- Code SHOULD NOT introduce traits with one implementation and no test double.
- Public `&mut self` SHOULD NOT be the default where returned values model the domain better.
- Code MUST NOT use blind `clone()` in hot paths.

---

## Code Review Checklist

Before approving any change:

- [ ] Any raw `String`, `i64`, or `Uuid` in meaningful signatures? Wrap in a newtype.
- [ ] Any validation after parsing? Remove it.
- [ ] Any `_ => {}` on your own enum? Match exhaustively.
- [ ] Any new dependency where the standard library would be enough? Remove or justify it.
- [ ] Any trait with a single implementor and no test double? Remove it.
- [ ] Any `clone()` in a hot path without justification? Restructure or document it.
- [ ] Any owned parameter or clone where a borrow would work? Prefer borrowing.
- [ ] Any `unsafe` block without minimal scope and documented invariants? Tighten or document it.
- [ ] Any `unwrap()` / `expect()` outside tests or bootstrap? Replace it.
- [ ] Any `anyhow` / `eyre` in reusable library code? Use a typed error.
- [ ] Any `assert!()` / `debug_assert!()` guarding what should be a type or typed error? Re-encode it.
- [ ] Any async path vulnerable to cancellation? Make it idempotent or transactional.
- [ ] Any async code blocking the runtime? Use async-aware APIs or `spawn_blocking`.
- [ ] Tests cover happy path, error path, and parsing? Add what is missing.

---

## Commands

```bash
cargo check
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

`cargo check` SHOULD run first to validate compilation quickly before heavier commands. This is usually 2–10x faster than a full build on large projects. `cargo clippy` and `cargo test` MUST run before considering any task complete.
