# ADR 0002: S0 Seed Auth Prototype Boundary

Status: accepted

## Context

S0 needs an account/auth route boundary so materials, imports, blobs, jobs,
annotations and progress are shaped as account-owned server state from the
first implementation slice. The final auth protocol is still open in
[`../systems/web-account.md`](../systems/web-account.md): Lumi may
choose OPAQUE/PAKE or seed-derived challenge signing.

S0 must not accept or store the raw seed phrase on the server, and it must not
hard-code a prototype so deeply that the final protocol requires rewriting
materials, reader commands or sync-oriented account state.

## Decision

Implement a replaceable seed-derived auth prototype boundary for S0:

- `/api/v1/auth/seed-prototype/register` accepts client-derived lookup and
  verifier material, plus optional profile display data.
- The core schema stores `SeedAuthPrototype` with an explicit
  `ReplaceableChallengeSigningSha256` algorithm marker.
- The server stores only lookup/verifier material, never the raw seed phrase.
- Account-owned records use stable `user_id`, so replacing the verifier
  protocol does not change material, revision, annotation, progress, blob or job
  ownership.

## Consequences

- S0 can exercise cloud-backed account-owned state without committing to the
  final PAKE/signing choice.
- The temporary algorithm marker makes migrations and audits explicit.
- This is not production auth hardening. It is a boundary contract and fixture
  path for early implementation.

## Alternatives

- Store a plaintext seed phrase for local development: rejected because it
  violates the account design and would train the wrong API shape.
- Skip auth/account routes until S1: rejected because S0 must prove web state is
  server/account-owned rather than browser-local.
- Choose OPAQUE/PAKE immediately: rejected for S0 because the protocol decision
  needs a separate security review and implementation slice.

## Compatibility

The S0 migration catalog includes `s0-0001-account-auth-boundary`. Future auth
work must add a migration that preserves `user_id` and replaces only the
verifier/challenge material and protocol routes.
