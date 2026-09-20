# Jev / TypeSafe API: selection experiment reference

## Verification and scope

This note was checked against fetched public TypeSafe documentation, including its Markdown documentation index. No `.env` or API key was read. No authenticated request was made. No SDK was installed. The examples below are documentation-verified request shapes, **not live-tested calls**. Public prices, models, and limits can change; account access and billing remain unverified.

## HTTP interface and enum decisions

- Endpoint: `POST https://api.typesafe.ai/v1/systemone`.
- Headers: `Authorization: Bearer <API_KEY>` and `Content-Type: application/json`.
- Required body: `state` (string, JSON object, or array), `model` (string), and `questions` (named question map).
- Enum decisions use `type: "choice"`, not chat completions or JSON Schema `enum`.
- Each Choice has `instructions` and `criteria`, a map from allowed option names to descriptions (or `null`). Option names and descriptions are visible to the model. The question-map key is not.
- Response: `model`, `answers` under the same question IDs, and `usage.input_tokens` / `usage.output_tokens`.
- Choice answer: `type: "choice"`, `choice` (highest-probability option), `probabilities` (all option names mapped to numbers summing to 1), and `confidence` (0–1).
- `confidence` is derived from the distribution, not identical to the winning probability. Do not substitute one for the other or invent its formula.

Minimal curl request, for later authorized execution only. This assumes `JEV_KEY` is already exported; it does not load the root `.env`:

```sh
curl --fail-with-body https://api.typesafe.ai/v1/systemone \
  -H "Authorization: Bearer ${JEV_KEY:?Set JEV_KEY securely before running}" \
  -H 'Content-Type: application/json' \
  --data '{
    "model": "jev-1.13.0",
    "state": "I was charged twice.",
    "questions": {
      "department": {
        "type": "choice",
        "instructions": "Which team should handle this?",
        "criteria": {
          "billing": "Payments, invoicing, refunds",
          "technical": "Bugs, outages, integrations",
          "other": "None of the listed teams fits"
        }
      }
    }
  }'
```

Read `answers.department.choice`, `answers.department.probabilities`, and `answers.department.confidence`. The response is not generated prose. Do not send requests with shell tracing enabled or log authorization headers.

Sources: [HTTP reference](https://docs.typesafe.ai/api.md), [Choice](https://docs.typesafe.ai/primitives/choice.md), [Confidence](https://docs.typesafe.ai/confidence.md).

## Official SDKs

Python distribution: **`typesafe-sdk`**. Import: **`typesafe_sdk`**.

```sh
uv add typesafe-sdk
# Alternative documented installation: pip install typesafe-sdk
```

The SDK normally reads `TYPESAFE_API_KEY`, **not `JEV_KEY`**. Its explicit `api_key=` parameter supports the experiment's existing variable. Minimal Python equivalent, for later authorized execution with the key already exported:

```python
import os
from typesafe_sdk import Choice, RetryPolicy, TypeSafeClient

with TypeSafeClient(
    api_key=os.environ["JEV_KEY"],
    model="jev-1.13.0",
    retry=RetryPolicy(max_retries=0),
) as client:
    response = client.system_one(
        state="I was charged twice.",
        questions={
            "department": Choice(
                instructions="Which team should handle this?",
                criteria={
                    "billing": "Payments, invoicing, refunds",
                    "technical": "Bugs, outages, integrations",
                    "other": "None of the listed teams fits",
                },
            )
        },
    )
    answer = response.choices["department"]
    print(answer.choice, answer.probabilities, answer.confidence)
    print(response.model, response.usage.input_tokens, response.usage.output_tokens)
```

`AsyncTypeSafeClient` supports `await client.system_one(...)`. The documented default HTTP-operation timeout is 10 seconds. Explicit settings override environment settings. The example disables automatic retries to make experimental attempts easier to budget; production callers should use bounded backoff.

JavaScript/TypeScript: `npm install @typesafe-ai/sdk` (Node.js 20+). Its methods differ: `TypeSafeClient.systemOne(...)`, with `choice(...)` from `@typesafe-ai/sdk`.

Sources: [Python quickstart](https://docs.typesafe.ai/sdk/python.md), [sync client signature](https://docs.typesafe.ai/sdk/python/api/clients/sync/client.md), [response types](https://docs.typesafe.ai/sdk/python/api/types/responses.md), [defaults](https://docs.typesafe.ai/sdk/python/api/constants.md), [JavaScript SDK](https://docs.typesafe.ai/sdk/javascript.md).

## Model IDs and limits

The fetched [models page](https://docs.typesafe.ai/models.md) lists:

| Item | Documented value |
| --- | --- |
| Versioned model | `jev-1.13.0` |
| Stable alias | `jev-latest`, currently points to `jev-1.13.0` |
| Preview alias | `jev-preview`, currently also points to `jev-1.13.0` |
| Total request context | 64k tokens: state plus all questions |
| Per-question context | 32k tokens: state plus the longest question |
| Choice option count | Up to 255 options per question |
| Rate limits | 250,000 tokens/second; 1,200 requests/minute |
| Input | Text only, including structured textual JSON; no images/audio/video |

Pin the version for comparable experimental results. Log the returned model ID as well. The docs say the response reports the versioned ID, although example responses sometimes display `jev-latest`; do not silently assume a version from an example.

`GET https://api.typesafe.ai/v1/models` uses bearer authentication. It lists account-available names, currently aliases; version IDs can be accepted even when absent from that listing. It was not called here.

Rate limits are explicitly dynamic and can change without notice. `429` means rate limited; `529` means temporarily overloaded. SDKs retry by default and honor `retry-after` when supplied. `401` indicates invalid/missing credentials; `422` means request validation failed.

No independent maximum number of questions or public account spending cap was verified. Do not invent either. Batch questions only while respecting both context limits.

## Price, usage, and experiment budget

The models page lists **$42 per billion input tokens = $0.042 per million input tokens**. Output tokens are free. Use returned usage, not output size or question count, to estimate charge:

```text
estimated_usd = input_tokens * 0.042 / 1_000_000
```

Examples at the published price:

| Total input tokens | Estimated charge |
| --- | --- |
| 1 million | $0.042 |
| 10 million | $0.42 |
| 100 million | $4.20 |
| 1,000 calls × 10,000 input tokens | $0.42 |

These are token-price estimates, not confirmed invoices or account terms. No free allowance, minimum charge, credit balance, or special account pricing was verified.

Suggested experiment controls (local design advice, not provider features):

1. Require explicit live-run opt-in. Start with one request, then a small pilot.
2. Set separate hard limits for attempts, concurrency, and estimated dollars.
3. Save `usage`, model, latency, and answers after each response. Never save the key.
4. Budget for retries and ambiguous timeouts. A timeout does not prove the provider did no billable work. Disabling automatic SDK retries makes attempt accounting clearer.
5. Reserve headroom before dispatch; usage is only available after a successful response. Character counts are not an exact tokenizer or a guaranteed budget bound.
6. Batch independent questions sharing the same state: the provider reads state once. Extra questions still cost input tokens. Do not batch unrelated large records merely to reduce request count.
7. Keep the SDK's request/response body logging off for sensitive records; its docs say secret headers are redacted but bodies are not.

## Selection-specific cautions

Choice selects one winner relative to other options; it does not establish absolute suitability. Include `none`/`other` when no candidate may fit, or use a separate `noul` question to estimate whether any candidate is suitable. A Noul returns `noul` (0–1), not a Choice-style confidence field. These question types are not interchangeable, and thresholds must be measured separately.

Use direct instructions and relevant state. Jev's published limitations include numeric precision, date comparison, indirection, irrelevant long context, adversarial content, and lack of guaranteed identities across separately worded questions. Keep exact arithmetic and deterministic eligibility rules in code. Evaluate calibration and selection quality on held-out experiment cases; confidence is not proof of correctness.

The Choice guide permits structured option descriptions, while the HTTP reference's field table describes string/null descriptions. The minimal examples here use string descriptions supported by both.

Sources: [Jev 1.13 limitations](https://docs.typesafe.ai/model-jaggedness/jev-1.13.md), [Choice](https://docs.typesafe.ai/primitives/choice.md), [models/pricing](https://docs.typesafe.ai/models.md). Full official index: [llms.txt](https://docs.typesafe.ai/llms.txt).
