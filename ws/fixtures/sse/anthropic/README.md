# Anthropic SSE fixtures

Recorded and synthetic streams used to test the decoder without a network
(master spec s7.6). Each file is a raw `text/event-stream` body.

- `text_only.sse` — a plain reply with no thinking.
- `thinking_with_signature.sse` — an extended-thinking reply. The signature
  arrives in its own delta just before the block closes, which is why the
  decoder holds the block until `content_block_stop`.
- `redacted_thinking.sse` — synthetic. The API returns a `redacted_thinking`
  block when its safety systems flag reasoning; the client must carry it back
  verbatim without understanding it.
- `error_mid_stream.sse` — an `overloaded_error` after text has already been
  emitted. Not retryable, because retrying would duplicate the text.
- `unknown_block_type.sse` — synthetic. A future block type this build does
  not recognise must still be preserved in the raw payload.
- `tool_use.sse` — synthetic. Two tool calls: one whose arguments stream in two
  `input_json_delta` pieces, and one with no deltas at all. A no-argument call
  can arrive like that, which is why the decoder emits calls at block stop.
