# MCP admission and response handoff

This note defines the enrolled-store and authority linearization protocol used by later MCP slices.

## State machine

```text
crate-private Enrollment::admit_current -> Pending<T>
crate-private Pending<T>::prepare_handoff (Tokio blocking pool) -> Prepared<T>
crate-private Prepared<T>::commit_with (async lifecycle wait, then one synchronous poll) -> T
```

These generic primitives are not public library API. Only PR2's bounded typed evidence operations and outer Tower adapter may call them. External safe callers cannot receive a `Connection`, construct a `Boundary`, produce or admit a `Snapshot`, prepare a `Pending`, or commit a `Prepared` value.

`Enrollment::read` returns an opaque `Snapshot<T>`. `Enrollment::admit_snapshot` consumes it and rechecks its originating enrollment and proof-owned revision under the original absolute deadline. `Pending<T>` is not cloneable and cannot reveal `T`. `Prepared<T>` is the cloneable internal response-extension carrier. Its clones share one atomic one-shot slot, one value, and one authority lease. A losing clone returns `store_unavailable/consumed` without waiting for the slot mutex.

The small authority lease retains both the atomic in-process admission count and a shared advisory `flock` on the enrolled `publication.lock` inode. Release closes the flock FD before atomically decrementing the count and notifying publishers. It takes no ordinary mutex. The generic value is not owned by the expiry service.

## Preparation and finalization

`prepare_handoff` claims `Pending` immediately. It moves the inert value to `spawn_blocking` and, by the original absolute deadline, acquires the enrolled connection gate and samples:

- state directory, stable publication lock, cache, and workspace identities;
- SQLite `SQLITE_FCNTL_HAS_MOVED`; and
- the current revision, which must equal the admitted revision.

This is the SQLite revision/`HAS_MOVED` proof. It occurs while the publication authority lease is retained. No connection, SQLite, watchdog join, or file-lock acquisition remains for final commit. The independent expiry job can revoke the small lease while `spawn_blocking` is queued or running, after the prepare future is canceled, or while an identity failure waits for lifecycle ordering. A decided identity failure releases publication before it waits to rotate the generation. Revision conflict and deadline expiry do not latch.

`Prepared::commit_with` may await only deadline-aware acquisition of the Tokio lifecycle sequencer. After acquisition it does not await and takes no blocking SQLite, file, or ordinary mutex lock. It:

1. reconciles the fast latch and checks cheap path/lock/cache/workspace identity, availability, and the original deadline;
2. invokes the read-only, fallible `prepare(&HandoffView)` callback to produce an inert plan;
3. regardless of the callback result, repeats cheap identity/latch and deadline checks;
4. gives identity/latch precedence over deadline, and deadline precedence over a callback error;
5. prechecks the absolute deadline, atomically claims the still-live authority lease, then samples the same deadline again while claimed; a late successful claim releases authority and fails before finalization;
6. invokes one infallible, nonblocking `finalize(LifecycleCommit, plan)` as the last authority mutation;
7. closes the flock and atomically releases admission without a mutex; and
8. reveals `T` without a later fallible authority step.

A prepare panic latches and rotates while serialized, releases guards safely, and resumes unwind. A finalizer or invalidation-teardown panic aborts the process because mutation may be partial. The token/view lifetimes and `Rc` phantom make them non-escaping and non-`Send`.

## Common lifecycle order

`Enrollment::lifecycle_transition` is the matching async prepare/finalize primitive for issuance, revoke, expiry, and budget mutation. `Enrollment::invalidate_with` latches and rotates, then runs infallible teardown under the same sequencer. All compliant authority code follows:

```text
lifecycle sequencer -> private grant mutex (try_lock) -> infallible finalizer
```

No compliant revoke or grant mutation can land between response validation and response finalization. If revoke or an identity latch owns lifecycle first, the response fails. If response finalization owns it first, that response commits first and the observer waits. A synchronous identity observer publishes the fast negative signal immediately but cannot return until guarded latch state is reconciled and the generation has rotated exactly once. Thus there is no best-effort-check race after a finalizer. Destructive store work must latch/teardown, release lifecycle, and only then wait for exclusive publication; it must never wait for publication while holding lifecycle.

## Stable publication and enrollment authority

One Store publication boundary accepts exactly one enrollment/lifecycle. Its in-process one-shot claim remains permanent, including after invalidation or after the enrollment facade is dropped. A separate exclusive, nonblocking kernel lease on `mcp-enrollment.lock` prevents a separately opened Store or process from enrolling the same state concurrently. The lease remains in the enrolled core while any work from that binding exists. After that core is dropped, a fresh Store may acquire the lease as daemon-restart semantics. Boundary construction and access are crate-private.

`publication.lock` and `mcp-enrollment.lock` are initialized together with mode `0600` only for new state or the bounded v1/v2 migration. Current state with either lock missing fails closed. Opens use `O_NOFOLLOW`, never repair or replace an inode, and check owner, mode, link count, device, and inode. Every boundary identity sample checks both stable lock paths. State-directory, cache, and workspace identities are anchored likewise.

`Store::publish` uses only `publication.lock`; the enrollment lease does not block normal publication. Publication holds exclusive in-process and cross-process authority around its SQLite transaction. A known pre-commit failure reopens it. A successful commit reopens it only after the final identity check. An ambiguous SQLite commit quarantines the canonical path and intentionally retains the publication flock until process exit.

The supported model requires cooperating writers to use `Store::publish` on a local Unix filesystem. Direct SQLite writes, inode-preserving overwrite, actors that ignore `flock`, and mutation after the last metadata sample are outside the claim. The child-process test qualifies real `flock` exclusion.

## Deadlines, cancellation, and expiry

Every phase carries the one original absolute deadline. Commit accepts no replacement deadline. PR2 must cap its effective request deadline to the minimum of protocol deadline and all credential/grant expiries.

Each admission creates one monotonic authority lease registered with a process-owned expiry service; conversion from Pending to Prepared does not replace it. Expiry atomically revokes only that small lease, closes its flock FD, and releases the atomic admission count. It never owns, locks, or drops generic `T`, so a retained clone or a blocking/reentrant destructor may retain inert memory but cannot retain publication or stall later expiry jobs.

The lease remains independently expirable while preparation is queued or running and while a commit waits for lifecycle serialization. The synchronous final handoff performs the only lease claim. Expiry-first makes that handoff fail. A successful claim samples the same absolute deadline again while claimed; an expired claim releases authority and fails before any finalizer or value disclosure. Only a claim proven to precede the deadline runs the trusted nonblocking finalizer and releases authority before the same poll returns `Ready`.

## Tower handoff

PR2's typed handler completes evidence work and a bounded, fully buffered response, awaits the crate-private `prepare_handoff`, and inserts `Prepared<Response>` into a harmless placeholder extension. The outer adapter removes it and polls `commit_with`. Lifecycle contention yields normally. The poll that acquires lifecycle performs prepare/finalize and returns `Poll::Ready(Response)` without another await or fallible authority operation. Missing extension or commit error fails closed. This linearization point is application response handoff, not socket delivery.
