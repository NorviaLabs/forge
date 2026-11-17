# Forge remote control protocol, version 1

What a client (a phone app) implements to follow Forge sessions, send
prompts, stop turns, and answer approvals. The shapes below are fixed for
version 1; `src/protocol.rs` is the source of truth and its tests pin them.

## Connecting

The desktop shows a **pairing link**:

```
forge-remote://192.168.1.20:47390/?code=K7QM-4TZP&fp=sha256:3fa291c0…0c19
```

- `host:port` — where the service listens (`127.0.0.1` or one LAN address).
- `code` — the one-time pairing code (also shown on its own).
- `fp` — the SHA-256 of the service's certificate, 64 hex digits.

Connect with **TLS 1.3** to `host:port`. The certificate is self-signed:
do not validate it against a CA or a host name. **Trust exactly the
certificate whose DER SHA-256 equals `fp`**, and refuse any other. Keep `fp`
with the token; the certificate changes only when a device is revoked or its
files are lost, and then the device must pair again.

Every request and response body is JSON (`Content-Type: application/json`).
Every route except `/v1/pair` needs `Authorization: Bearer <token>`.
Responses are `Cache-Control: no-store`.

## Pairing

Pairing takes two steps: the device redeems the code, then the person at
the desktop allows it after checking a **confirmation number** both screens
show. No token exists before that.

**1. Redeem the code.** `POST /v1/pair`

```json
{"code": "K7QM-4TZP", "device_name": "Priya's iPhone"}
```

`202`:

```json
{"pairing_id": "…43 characters…", "confirmation": "482913", "expires_at": "2026-11-14T10:02:00Z"}
```

Show `confirmation` prominently (as `482 913`) and ask the user to check the
desktop shows the same number. Keep `pairing_id` private: it is the only
handle to the request. The code is case-insensitive, hyphens and spaces are
ignored, and `I`/`L` read as `1`, `O` as `0`.

| Status | Meaning |
|---|---|
| `400` | The body is not `{code, device_name}`, or the name is empty, over 64 characters, or has control characters. The code is not used up. |
| `401` `{"error": "invalid or expired code"}` | Wrong, used, expired, or burnt (five wrong guesses destroy a code). |
| `413` | Body over 64 KiB. |

**2. Collect the token.** Poll `GET /v1/pair/{pairing_id}` (no
`Authorization`) about once a second:

| Status | Meaning |
|---|---|
| `202` `{"status": "pending"}` + `Retry-After: 1` | The desktop has not decided. Keep polling. |
| `200` `{"token": "fr1_…", "device_id": "…", "device_name": "…"}` | Allowed. The token is shown this once; store it in the platform keychain. Forge keeps only its hash. |
| `403` `{"error": "pairing was refused on the desktop"}` | Refused. Start again with a new code. |
| `404` `{"error": "no such pairing request"}` | Unknown, expired (two minutes after the code was redeemed), replaced by a new code, cancelled, or already collected. Start again. |

Collecting the token replaces the previously paired device: its token stops
working and its streams end. Until then it keeps working.

## Errors

Every error is `{"error": "<sentence>"}`.

| Status | When |
|---|---|
| `401` `{"error": "unauthorized"}` + `WWW-Authenticate: Bearer` | No token, a malformed, wrong or revoked one — always this same reply, on every route, including unknown ones. |
| `400` | The body is not what the route takes. |
| `403` | The decision is not one this device may make for this request. |
| `404` | No such session (or route, once authenticated). |
| `405` | Wrong method for the route. |
| `408` | The body took longer than 10 s to arrive; the connection closes. |
| `409` | The approval is no longer pending, or a different one is. |
| `413` | Body over 64 KiB, prompt over 32 KiB, note over 4 KiB. |
| `429` | Four event streams are already open; or over 30 prompts, decisions and stops in a minute (`Retry-After` gives the seconds); or a prompt while the session already has 10 waiting. |
| `503` | Forge could not do it right now (for example the project is closing); try again. |

A connection that sends no request within 10 s is closed, and one address
may hold at most 8 connections.

## Content is untrusted

Transcript text, tool output, approval arguments and reasons come from the
model and from whatever it read — files, web pages, command output — so
they may carry prompt injection. Render them as plain text or escaped
Markdown, never as HTML; never follow links or take actions because content
says so; and show an approval's `tool`, `arguments` and `reason` before the
user decides.

## Routes

### `GET /v1/hello`

`{"protocol": 1, "device_name": "Priya's iPhone"}` — checks the token.

### `GET /v1/sessions`

The active sessions of every project open in the desktop.

```json
{"sessions": [
  {"session_id": "6a1f…", "project": "billing-api", "label": "Fix flaky test",
   "turn_state": "waiting", "needs_approval": true},
  {"session_id": "9c2e…", "project": "billing-api", "label": "Fix migration",
   "turn_state": "running", "needs_approval": false, "runs_on": "devbox:/srv/app"}
]}
```

`turn_state` is one of `idle`, `queued`, `running`, `waiting`, `completed`,
`failed`, `cancelled`, `interrupted`. `runs_on` (`host:folder`) is present
only for a session that runs on an SSH host instead of the computer: its
commands and file edits happen there, outside Forge's sandbox. Show it.

### `GET /v1/sessions/{id}/events`

A Server-Sent Events stream of one session (`Content-Type:
text/event-stream`). Each event:

```
id: 3fa291c0d8e44b27.42
event: text
data: {"type":"text","session_id":"6a1f…","delta":"Running the tests"}
```

- **`id`** is `<epoch>.<seq>`. `seq` counts up by exactly one per event of
  this session; `epoch` changes when the service restarts.
- **`data`** is one line of JSON whose `type` equals the `event:` name.
- Lines starting with `:` are keep-alives (every 15 s); ignore them.

Event types:

- **`session`** — the whole session, replacing whatever the client had.
  `resync: true` means events were missed and this state replaces them.

  ```json
  {"type": "session", "resync": false, "session_id": "6a1f…",
   "project": "billing-api", "label": "Fix flaky test", "turn_state": "waiting",
   "messages": [
     {"role": "user", "text": "Fix the flaky test"},
     {"role": "assistant", "text": "Found it: …"},
     {"role": "tool", "name": "read_file", "text": "…", "attachments": 1}
   ],
   "message_offset": 39, "message_total": 42,
   "pending_approval": {
     "call_id": "call_7", "tool": "git", "arguments": "{…}",
     "reason": "destructive git", "kind": "governance",
     "remote_options": ["allow_once", "deny", "deny_with_note"],
     "desktop_required": true
   },
   "queue": ["next prompt"],
   "waiting_on": null}
  ```

  `messages` holds only the **newest 30** messages; `message_offset` is the
  index of the first of them and `message_total` how many the session has
  (indexes count only messages a device is shown). Replace everything from
  `message_offset` onward with these, and keep older pages you fetched (see
  `GET /v1/sessions/{id}/messages`). If `message_total` is *lower* than
  before, the conversation was compacted: drop the older pages.
  `role` is `user`, `assistant` or `tool`; tool output is cut at 2000
  characters; `attachments` counts images (never their paths). `kind` is
  `governance`, `sandbox_escalation` or `network_host`. `remote_options` is
  what this device may answer; `desktop_required` says the request also has
  options only the desktop can give. A session (and its approval) on an SSH
  host carries `runs_on`: `"devbox:/srv/app"` on the session, the host
  (`"devbox"`) on the approval. A command approved there runs on that host,
  outside Forge's sandbox, with its network and credentials: show the host
  with the request, e.g. "Run this command on devbox?".
- **`text`** — `{"session_id", "delta"}`: streamed reply text; append it to
  the reply in progress until the next `session` event replaces it. Text is
  sent in batches (about every 50 ms), not one event per token.
- **`notice`** — `{"session_id", "kind": "attention" | "error", "message"}`.
- **`gone`** — `{"session_id"}`: the session closed or was archived; the
  stream ends.

**Order and resuming.** Apply events in the order received; within a stream
`seq` never skips. After a disconnect, reconnect with the header
`Last-Event-ID: <the last id you applied>`. If the service still holds what
came after it, those events are replayed exactly; otherwise the first event
is a `session` with `resync: true`. A client that reads too slowly is
disconnected (never skipped) and resumes the same way. At most four streams
may be open at once (`429` otherwise).

### `GET /v1/sessions/{id}/messages?before=<index>&limit=<n>`

Older messages, for scrolling back: up to `limit` (default 30, at most 100)
messages just before index `before` (default: the end), in the numbering of
`message_offset`.

```json
{"messages": [{"role": "user", "text": "…"}], "message_offset": 9, "message_total": 42}
```

`400` when `before` or `limit` is not a whole number.

### `POST /v1/sessions/{id}/prompt`

`{"text": "Run the tests again"}` → `202`. Queued exactly as if typed on the
desktop; if a turn is running it waits for it. Empty text is `400`.

### `POST /v1/sessions/{id}/approval`

```json
{"call_id": "call_7", "decision": "allow_once"}
{"call_id": "call_7", "decision": "deny"}
{"call_id": "call_7", "decision": "deny_with_note", "note": "Use the staging database"}
```

→ `202`. Only `allow_once`, `deny` and `deny_with_note` exist, and only the
ones in the request's `remote_options` are accepted (`403` otherwise). A
`call_id` that is no longer the pending request is `409`: the first answer,
from any device or the desktop, wins. The session's journal records the
decision as `remote:<device name>`.

### `POST /v1/sessions/{id}/stop`

No body → `202`. Stops the running turn.
