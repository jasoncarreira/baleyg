# Selection research evidence

- [Smoke results](SMOKE-RESULTS.md) and [hard-question results](HARD-RESULTS.md)
- [Questions](QUESTIONS.md) and [hard-question rubric](HARD-QUESTIONS.md)
- [Jev interface notes](JEV-API.md) and [ACP setup research](ACP-SETUP.md)
- [Offline tools and review commands](../../../tools/selection/README.md)

All original JSON evidence is preserved byte-for-byte. ACP setup notes and the first
failed hard-question log have explicitly labelled local-path redactions, with exact
originals retained privately. Other paths and commands embedded in evidence describe
the original layout, not current defaults. Prose paths beginning `tests/` are repository-relative.
Saved packets, provider results, logs, screenshots, reviews and accounting files are
under [`tests/fixtures/selection`](../../../tests/fixtures/selection/).

The actual closed provider ledger is
`tests/fixtures/selection/outputs/budget.json`. It and all accounting siblings were
moved without byte changes. No new inference, reset, replacement ledger or additional
spend is authorized. Review existing evidence; do not run provider entry points.
