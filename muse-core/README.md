# `muse-core` Crate

Shared Data Models, Protocol Framing, and Common Utilities.

`muse-core` is the shared foundational library utilized by both `muse-hub` and `muse-agent`. It provides industry-standard API types, serialization and deserialization schemas for OpenAI and Anthropic formats, and binary framing helpers for internal Unix Domain Socket communication.

---

## Core Components

1. **OpenAI Standard Models (`openai`):**
   - Full request and response types for `/v1/chat/completions`.
   - Server-Sent Events (SSE) streaming chunk models (`ChatCompletionChunk`).
   - First-class support for Tool Calling and Function Calling schemas (`ToolCall`, `FunctionCall`).
   - Image generation payload types (`/v1/images/generations`).

2. **Anthropic Standard Models (`anthropic`):**
   - Request and response types for `/v1/messages`.
   - Comprehensive content blocks (text, base64 images, tool use, and tool results).

3. **UDS Wire-Protocol Framing (`uds`):**
   - Framing implementation tailored for the Hatch sandbox environment.
   - 4-byte Big-Endian Length Prefix framing to prevent stream fragmentation across Unix Domain Sockets.

4. **Shared Errors & Auth Utilities:**
   - Standardized cluster error classifications.
   - Bearer token parsers and credential validation helpers.
