# ADR-001: Money representation

- **Status:** Proposed — needs acceptance by the tech lead and one other engineer (E00-T03)
- **Date:** 2026-11-05
- **Source:** [docs/backlog/03-decisions.md](../backlog/03-decisions.md), PRD §8–§14

## Context

The PRD (docs/PRD.md) fixes the product constraints this decision serves: offline-first local
checkout, atomic local transactions, at-least-once sync without duplicate effects, decimal-safe
money, and hardware independence. This ADR records the engineering choice the backlog assumes.

## Decision

- Money is an **`i64` count of minor units** (cents, fils, etc.). ISO-4217 currency
  metadata comes from the business: the code and the exponent (0–4).
- **Each business has exactly one currency in the MVP.** Domain `Money` values therefore do
  not carry a currency. The currency is fixed on the business, and each sale snapshots its
  `currency_code` and `currency_exponent`. If multi-currency is added later, the type becomes
  `Money { minor, currency }`.
- Intermediate calculations, such as tax on an inclusive price or percentage discounts, use
  `rust_decimal::Decimal`. They are rounded back to minor units **once**, at the points named
  in ADR-004.
- On the TypeScript side, money is a decimal **string of minor units** (e.g. `"1250"`) in
  IPC payloads. The UI only formats it. It never does arithmetic on money for business
  purposes.

## Consequences

- Cost of change: very high after Stage 1.
- Tasks citing this ADR in `docs/backlog/tasks/` must be re-checked if it is superseded.
