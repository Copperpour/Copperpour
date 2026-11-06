### Over HTTP, progress arrives on the call's own SSE response (minor release)

Over the Streamable HTTP transport, a `tools/call` that carries
`_meta.progressToken` from a client whose `Accept` header lists
`text/event-stream` is now answered with an SSE stream of its own: the call's
`notifications/progress`, then the JSON-RPC response as the last event, then
the end of the stream. Only that client sees them.

Progress no longer goes to the `GET /mcp` streams, which every client shares:
a client could see progress for a token it never sent. Those streams keep
carrying the notifications not tied to a request, such as
`notifications/tools/list_changed`.

Every other request, including a `tools/call` without a token or from a client
that accepts only `application/json`, still gets a plain JSON response.
Callers that follow the MCP Streamable HTTP specification need no change; stdio
is unchanged.
