// Real, enforced limits this module derives from — not arbitrary numbers:
//
// 1. HTTP ingress: `cell/src/chat.rs` routes requests with
//    `DefaultBodyLimit::max(16 * 1024 * 1024)`; `cell/src/operator.rs` uses the
//    same cap for the VM-daemon router. Bodies at or above 16 MiB are rejected
//    with 413 before reaching the chat bridge.
// 2. Pi RPC framing: `cell/src/pi.rs` errors with "Pi record exceeded 16 MiB"
//    on any single JSON command line of 16 MiB or more. The prompt frame
//    carries the base64 image data inline, so its size tracks the request body.
// 3. Provider limits: Anthropic rejects image blocks over 5 MiB (and over
//    8000x8000 px); OpenAI is more lenient (20 MiB per image, 50 MB request).
//    5 MiB per image is therefore the binding provider-side limit.

/** Largest single image accepted, in bytes (provider hard limit). */
export const MAX_IMAGE_BYTES = 5 * 1024 * 1024;

/** Largest outgoing POST body, in bytes: the 16 MiB ingress and RPC-frame caps
 * minus 64 KiB of headroom for the JSON envelopes that differ between the HTTP
 * body and the Pi frame. */
export const MAX_MESSAGE_BODY_BYTES = 16 * 1024 * 1024 - 64 * 1024;

/** Exact UTF-8 byte length of a string (base64 and JSON are ASCII, but user
 * text is not, and the server caps count bytes, not UTF-16 code units). */
export function utf8Length(text) {
  return new TextEncoder().encode(text).length;
}

/** Human-readable MiB size for error messages. */
export function mib(bytes) {
  return (bytes / (1024 * 1024)).toFixed(1);
}
