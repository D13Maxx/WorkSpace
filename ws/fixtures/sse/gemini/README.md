# Gemini SSE fixtures

Streams used to test the decoder without a network. Gemini
has no named SSE event types: every `data:` line is a whole
`GenerateContentResponse`, and the new content is in
`candidates[0].content.parts`.

- `text_only.sse` — a plain reply across several chunks.
- `thinking_with_signature.sse` — thought parts (`thought: true`) followed by
  the answer, with a `thoughtSignature` that must be replayed verbatim.
- `signature_on_plain_part.sse` — the case that breaks a naive decoder. A
  `thoughtSignature` sits on an ordinary, non-thinking part. Treating "has a
  signature" as "is thinking" would mislabel the text as reasoning and hide it
  from the user; ignoring signatures on non-thought parts would drop it from
  replay.
- `max_tokens.sse` — generation stopped at the output cap.
- `error_mid_stream.sse` — a quota error after text has streamed.
