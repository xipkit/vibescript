# Glue programs

Forty small, deterministic workflows with behavioral tests. Run them with:

```sh
vibes test --profile low corpus/glue
```

The CLI integration suite runs the same command during `./scripts/cargo test
--offline --workspace`. Each directory is an independent module root. Tests
require the program inside their test function, since test files contain only
declarations. Capability operations are supplied as typed blocks or script
doubles; no network access or host credentials are needed.

[The evaluation report](../../docs/authoring-evaluation.md) explains the first
attempts, repairs, limitations and language proposals. `evaluation.jsonl` stores
all 40 original drafts and subsequent attempts, plus separately seeded spelling
exercises. Those drafts are data, so the normal test runner never discovers and
executes the intentionally failing versions. The replay script verifies their
source hashes before writing them to a fresh output directory.

| Program | Workflow |
| --- | --- |
| [api_contacts](api_contacts/api_contacts.vibe) | Normalize a JSON contact API response and serialize a typed projection |
| [api_invoice](api_invoice/api_invoice.vibe) | Flatten nested JSON invoice lines into an outbound payload |
| [page_numbers](page_numbers/page_numbers.vibe) | Collect numbered JSON pages until has_more is false |
| [page_cursors](page_cursors/page_cursors.vibe) | Follow optional cursors with a maximum page budget |
| [webhook_validation](webhook_validation/webhook_validation.vibe) | Validate typed webhook structure and positive amount |
| [webhook_projection](webhook_projection/webhook_projection.vibe) | Ignore extra webhook fields while requiring an event envelope |
| [retry_delivery](retry_delivery/retry_delivery.vibe) | Retry a supplied capability operation, with an attempt limit |
| [capability_fallback](capability_fallback/capability_fallback.vibe) | Fall back when a capability read raises an ordinary error |
| [delivery_batch](delivery_batch/delivery_batch.vibe) | Collect per-recipient failures without losing successful deliveries |
| [regional_totals](regional_totals/regional_totals.vibe) | Aggregate order cents by region |
| [daily_report](daily_report/daily_report.vibe) | Produce sorted status count lines |
| [top_metrics](top_metrics/top_metrics.vibe) | Rank service latency records and calculate an average |
| [email_template](email_template/email_template.vibe) | Render a strict notification template with nested context |
| [csv_export](csv_export/csv_export.vibe) | Escape quotes and commas in an outbound CSV |
| [format_summary](format_summary/format_summary.vibe) | Format counts, percentages, and integer cents for a report |
| [deadline](deadline/deadline.vibe) | Compute a deterministic UTC timeout from an input timestamp |
| [expiry_policy](expiry_policy/expiry_policy.vibe) | Compare a credential expiration against a fixed clock and grace |
| [duration_report](duration_report/duration_report.vibe) | Summarize queue delays as seconds and ISO duration |
| [money_invoice](money_invoice/money_invoice.vibe) | Sum typed money lines and serialize cents and currency |
| [money_discount](money_discount/money_discount.vibe) | Apply an integer percent discount without floating point |
| [enum_events](enum_events/enum_events.vibe) | Parse JSON event states and dispatch exhaustively |
| [enum_routing](enum_routing/enum_routing.vibe) | Choose a queue by a typed priority enum |
| [rate_limiter](rate_limiter/rate_limiter.vibe) | Track a per-service request budget with typed class state |
| [work_queue](work_queue/work_queue.vibe) | Drain a typed record queue |
| [required_pricing](required_pricing/required_pricing.vibe) | Use a separately required pricing helper |
| [required_states](required_states/required_states.vibe) | Use an enum and a returned private class from a required file |
| [log_parser](log_parser/log_parser.vibe) | Parse service status and elapsed time from log lines |
| [log_redaction](log_redaction/log_redaction.vibe) | Redact token query parameters without rewriting unrelated fields |
| [user_index](user_index/user_index.vibe) | Index typed records by id and resolve a batch of ids |
| [record_snapshot](record_snapshot/record_snapshot.vibe) | Update nested record arrays without modifying the original value |
| [inventory_join](inventory_join/inventory_join.vibe) | Join orders to a dictionary of stock records |
| [dedup_events](dedup_events/dedup_events.vibe) | Keep the first delivery of each event id |
| [request_batches](request_batches/request_batches.vibe) | Build bounded API batches and include a partial final batch |
| [flatten_response](flatten_response/flatten_response.vibe) | Flatten nested repository issue responses |
| [config_overrides](config_overrides/config_overrides.vibe) | Merge optional configuration values after removing nils |
| [dynamic_scalar](dynamic_scalar/dynamic_scalar.vibe) | Normalize a dynamic JSON scalar using explicit narrowing |
| [slug](slug/slug.vibe) | Normalize an external label to an ASCII routing slug |
| [time_windows](time_windows/time_windows.vibe) | Group timestamps into fixed UTC hourly report keys |
| [job_state](job_state/job_state.vibe) | Move a job through typed states and serialize its status |
| [reconciliation](reconciliation/reconciliation.vibe) | Reconcile expected and observed service counts into mismatch records |
