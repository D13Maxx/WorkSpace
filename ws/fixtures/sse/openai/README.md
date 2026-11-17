# OpenAI Responses SSE fixtures

Streams used to test the decoder without a network. Each
file is a raw `text/event-stream` body.

- `text_only.sse` — a plain reply, no reasoning.
- `reasoning_with_encrypted.sse` — a reasoning item carrying
  `encrypted_content`, followed by the message it produced. The pair must be
  replayed together and in this order.
- `partial_encrypted_on_added.sse` — the trap. `response.output_item.added`
  carries a **truncated** `encrypted_content`; only
  `response.output_item.done` has the whole thing. A decoder that reads the
  `added` event stores a blob the API later rejects.
- `incomplete_max_tokens.sse` — generation stopped at the output cap.
- `error_mid_stream.sse` — a rate-limit error after text has streamed. Not
  retryable, because retrying would duplicate the text.
