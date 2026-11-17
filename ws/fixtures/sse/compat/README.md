# OpenAI-compatible (Chat Completions) SSE fixtures

Streams used to test the decoder without a network. This
protocol ends with a literal `data: [DONE]` sentinel rather than by closing
the body.

- `text_only.sse` — a plain reply ending with `[DONE]`.
- `reasoning_content.sse` — a server that exposes reasoning through the
  non-standard `reasoning_content` field. It is shown to the user and then
  dropped: there is no signature, so nothing here is replayable.
- `no_done_sentinel.sse` — a server that just stops after `finish_reason`.
  Common enough among compatible servers to treat as a clean close.
- `usage_after_finish.sse` — a final usage-only chunk arriving *after*
  `finish_reason`, which is why the decoder does not finish on that field.
- `error_mid_stream.sse` — a server error after text has streamed.
