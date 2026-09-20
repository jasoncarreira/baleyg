# Offline question-slice validation

- **58 Rust integration tests passed**, including 9 planner, 7 Jev protocol and 5
  question HTTP tests. Existing native tests remain green.
- **12 dependency-free UI regression tests passed**: delayed obsolete failures,
  source/query/question generation guards, current error behavior, and exact UTF-8
  export Blob boundaries at 176,000 and 176,001 bytes.
- Formatting and clippy (`-D warnings`) passed. The embedded client was rebuilt.
- Real feature-factory snapshot exports: transition 39,719 bytes / 11 candidates;
  withRunJsonLock 155,248 / 89; writeProtectedFileAtomic 100,414 / 49.
  Questions used identical test wording and explicit literal focus terms. These are
  payload measurements, not evidence that the local preview answers the questions.
- Browser checked: anchored per-branch expansion, focused-save guard, source navigation,
  export/download, synthetic response import, and return to raw navigation.
  A delayed obsolete import 409 did not clear a newer valid focused preview.
- Actual browser download was 155,252 bytes for a different lock question. It was
  compact, source-complete and below the guard. No inference request was sent.
- Mobile viewport/document widths both measured 390px. Desktop and mobile screenshots
  show the **local literal preview**, not provider output.
- User inspector restarted on **8877**, still revision 2, with the same token and
  unchanged saved views and annotations. Separate testing used 8878 and isolated state.

Two review defects were fixed: pretty-printed exports could exceed the compact byte
limit, and obsolete failed requests could invalidate newer UI state. Both have durable
regression coverage. One browser console 409 was deliberately injected for the race test.

No paid calls, provider credentials, or inference budget changes were used. Imported
responses in browser tests were synthetic protocol fixtures, not actual Jev selections.
Live orchestration and quality evaluation remain deferred pending separate authorization.

[Machine-readable measurements](question-validation.json) ·
[Desktop](images/question-focus-desktop.png) · [Mobile](images/question-focus-mobile.png)
