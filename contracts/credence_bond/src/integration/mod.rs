/// Integration tests for bond lifecycle (#47).
///
/// This module groups the end-to-end test suites that exercise the
/// `contracts/credence_bond` contract through its public entry points. The
/// submodules are deliberately split by concern so that boundary and
/// recovery coverage can grow without making the lifecycle suite unreadable.
//
/// ### Invariants
///
/// - Every test in this module must be deterministic: no wall-clock time,
///   no randomness, and no implicit order dependencies between tests.
/// - Adverse cases (rejection, boundary, retry, stale, permission) are
///   covered alongside the happy path so a silent regression in failure
///   handling is caught by the same suite that guards normal operation.
/// - Tests must not mutate shared global state outside of the contract
///   storage they explicitly seed, so they remain safe under concurrent
///   execution by the test runner.

/// Happy-path and regression coverage for the bond lifecycle (#47).
///
/// Covers bond creation, activation, release, and the regression cases
/// discovered while implementing the lifecycle. Boundary and recovery
/// scenarios for this suite live in the dedicated modules below.
mod test_bond_lifecycle;

/// Governance authorization and state-transition coverage.
///
/// Ensures that only authorized callers can drive governance transitions and
/// that rejected attempts leave the observable state unchanged.
mod test_governance;

/// Boundary-case coverage for valid, duplicate, and out-of-range inputs.
///
/// These tests pin down the exact edge behavior (minimum/maximum amounts,
/// duplicate identifiers, and zero/empty inputs) so that validation cannot be
/// silently weakened by a future refactor.
mod test_boundary;

/// Retry, partial-failure, and recovery coverage.
///
/// Exercises the paths where an operation is repeated after a failure or
/// where a previously persisted state is reloaded, asserting that no user
/// data is lost and no inconsistent state is observable.
mod test_recovery;
