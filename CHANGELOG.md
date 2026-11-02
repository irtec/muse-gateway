# Changelog

All notable changes to this project are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning is [SemVer](https://semver.org/).

## [0.1.12] - 2026-11-02

The quota figures were wrong in a way that mattered: the agent counted tokens in
memory, so every restart reset them to zero and every alert said `no traffic yet`
even on a node that had been serving for hours. The platform does publish the
real allowance — just not on the sockets the bridge was probing.

### Fixed

**Agent — real allowance**

- Quota is sampled from the platform's own `subscription-status status`, which
  reports the percent of the weekly limit consumed, the exact reset time, and any
  purchased tokens left. Probes of `space-inference.sock` with `quota`, `usage`,
  `status`, `limits`, `balance` and `credits` all return EOF or a connection
  reset; `space-privileged.sock` behaves the same way.
- Sampling runs on a timer (60s) with a 30s timeout and a cached result, so
  `/health` never blocks on a subprocess. A failed sample reports `available:
  false` — "unknown", never "fine".
- `/health` carries a `quota` block alongside `usage`.

**Hub — skip until the reset, not for a blind window**

- A node whose allowance is spent is skipped by round-robin until the reset time
  the platform printed, plus a 60s cushion. The previous 5-minute blind window
  kept re-testing a known-dead account 12 times an hour.
- The fallback window still applies when no reset time is available (older agent,
  failed sample), so a node is never stranded out of rotation.
- The skip is only lifted by a fresh platform sample or by the reset time passing.
  The in-memory `usage` flag alone can no longer clear it.
- Reset times are parsed from `<Mon> <day> at <h>:<mm> <AM|PM> UTC`; a date that
  would land in the past rolls to the next year.

**Hub — alerts**

- The fleet overview and the online alert show the real allowance, worded like
  the app's usage card: `Free plan · 28% used · resets Nov 4 · bonus 999M tokens
  left`, instead of inferring credit state from counters that reset.
- Bonus tokens are a top-up balance, not part of the weekly limit, so the line
  only mentions them when the platform reports some. Without a bonus balance the
  percentage is the whole story.
- A spent allowance reads `LIMIT REACHED`; an unsampled one reads `unknown`.
- Fleet lines drop the redundant `Healthy` label and carry the version inline:
  `• 🟢 VM-02 (Backup) v0.1.12: 8% used · 1B tokens left`. Token counters are
  only appended once the node has served traffic, so a fresh process no longer
  prints `no traffic yet` beside a real allowance.

### Added

- Tests for the status parser (credits left, at limit, unrecognized output),
  reset-time parsing including the year rollover, the skip deadline both with
  and without a reported reset, and the alert wording with and without a bonus
  balance. 82 tests total.

## [0.1.11] - 2026-11-02

Failover is the hub's job, and quota is now a routing fact instead of a surprise.
A client could previously receive a `422`, a `503`, or an empty body that it had
to retry itself, and a node with no credits left kept receiving traffic.

### Fixed

**Hub — failover policy**

- Retryable failures now fail over before the client sees anything: any `5xx`,
  plus `408`, `422` and `429`. `422` in particular used to be terminal, so a
  transient upstream failure ended the caller's turn.
- The hub tries each worker once instead of retrying whichever node
  round-robin happened to hand it. Retry budget raised from 3 to 5 attempts.
- When every worker has failed, the reply is `503 upstream_unavailable` with a
  message saying the hub already retried everything — not `502 bad_gateway`,
  which read like the caller's request was at fault.
- Only real faults (`5xx`, connection errors) take a node out of rotation. A
  retryable `4xx` is retried elsewhere without marking the node unhealthy.

**Hub — quota routing**

- A worker that reports `insufficient_quota` is remembered as exhausted and
  skipped by round-robin, so requests no longer waste a round trip on an account
  with no credits. The node stays healthy: its services are fine.
- The skip expires after 5 minutes so a refilled account rejoins on its own; a
  `/health` probe that reports credits back clears it immediately.
- Round-robin now rotates in worker definition order rather than `HashMap`
  order, so rotation is stable and predictable.

### Added

- Per-node token accounting. The agent counts `usage` from every inference and
  exposes it on `/health`; the sandbox publishes no credit balance, so this is
  the real consumption signal available.
- Telegram alerts carry the numbers: the fleet overview shows
  `<tokens> tokens / <n> reqs` per node and flags `quota out`; the online alert
  adds a `Token Usage` and `Quota` line.
- Tests for the `422`/`503` failover paths, exhausted-node skipping, the
  all-failed `503` reply, and usage formatting. 73 tests total.

## [0.1.10] - 2026-11-02

Tool calling no longer fails silently. A web-search client could ask for a tool,
receive `HTTP 200` with an empty `content`, and stall — or receive `422` and treat
the turn as permanently broken. Both came from the same place: the sandbox runs
native function calls itself and returns the result in a channel the bridge never
reads, so the gateway saw a successful response with nothing in it.

### Fixed

**Agent — tool calling**

- Tool calls are forced into plain message text. A system-prompt directive
  forbids the native function-calling API, so the model writes its call as JSON
  in `content` where the gateway can see and parse it. Success on a tool-calling
  prompt went from 0/3 to 6/6 against the live sandbox.
- A response that carries neither a tool call nor text is retried twice, then
  answered with `502` instead of an empty `200`. An empty `200` left the client
  waiting on a response that had no content to render.
- Transient sandbox failures (`ok:false`, e.g. "Web artifact inference failed.")
  are retried twice, then mapped to `502` instead of `422`. `422` is terminal for
  the client; `502` lets the hub fail over to another worker.
- The Anthropic `/v1/messages` path gets the same treatment: retries before
  giving up, and `502` in place of `422` for both the upstream-error and the
  empty-content cases.

### Added

- Tests covering the empty-content retry, the `502` mapping for transient
  upstream failures, and a plain-text tool call still parsing into
  `tool_calls`. 65 tests total.

## [0.1.9] - 2026-11-02

Recovery is the theme of this release. A replaced worker could come back with
a healthy agent, a listening `sshd`, and no tunnel at all, because the process
supervisor and `run.sh` disagreed about who owns `frpc` — and the probe that
should have noticed was blind to the wreckage.

### Fixed

**Agent — tunnel supervision**

- A worker no longer runs two `frpc` processes. `run.sh` starts one with
  `setsid` (`ppid=1`) while the supervisor started its own; the duplicate
  registered the same proxy names, was rejected with `session shutdown`, exited
  `1`, and was respawned every 5 seconds — 12 spawns per minute, each leaving a
  defunct child behind. The supervisor now adopts a running `frpc` instead of
  spawning a second one.
- The supervised child is reaped every cycle, so a crash no longer leaves a
  `Z` process under the agent.
- Repeated spawn failures back off exponentially (1s to 32s) instead of
  retrying at the full check interval.
- The `frpc` liveness probe matches `argv[0]` in `ps -eo stat,args` and skips
  zombies. `pgrep -x frpc` matches a defunct process because `/proc/<pid>/cmdline`
  is empty but the name remains in the table, so the watchdog believed the
  tunnel was alive and never ran recovery. The same fix is applied to `run.sh`
  and to the platform watchdog command.
- `run.sh` now refreshes itself from the installer mirror on hash mismatch.
  The binary self-updated through sync but the recovery runner never did, so a
  fix to its probes could not reach the fleet at all.

**Installer — fresh installs**

- `install.sh` no longer aborts on an undefined `${BASE_DIR}` reference, which
  killed a fresh install at step 1 under `set -euo pipefail`.
- Credentials are required, not templated: `CLUSTER_TOKEN`,
  `FILE_SERVER_USER`/`SALT`/`HASH`, `EDGE_HOST` and `FRP_TOKEN` must be supplied,
  and the installer refuses to write a config containing placeholders.
- `sync role` is derived from `VM_ID` (`master` on `*1`, `replica` otherwise).
- `muse-agent.toml` and `frpc.toml` are written to a temporary file and renamed,
  so a partial write cannot make the `[ ! -f ]` guard skip regeneration forever.
- A stale mirror template is rejected instead of silently substituting nothing.
- The health check requires `ok:true` and exits non-zero when the node is
  degraded, rather than reporting PASS for a node whose storage is dead.
- Binaries and `run.sh` are downloaded to a temporary name and renamed, avoiding
  `ETXTBSY` and a truncated recovery runner.
- `deploy.sh` installs the hub binary via `.new` + `mv`, actually restarts the
  service, and seeds `/etc/muse-hub/config.toml` once on a first install.

**Cluster — failover correctness**

- The hub no longer selects a worker with `storage` down as the file-server
  standby. Candidates require `storage`; the legacy fallback applies only when
  no worker advertises it at all.
- Placeholder (all-zero) file-manager credentials are detected and reported as
  `storage: false`. They can never match a PBKDF2 digest, so every failover
  login returned `401` on the standby node.
- `HTTP 429` with `x-muse-error: insufficient_quota` is treated as a billing
  state, not a fault: the hub fails over without marking the worker failed.
  Real `5xx` still does.
- `version_hash` is recorded only after every file in a sync batch lands
  intact. It used to be stored even when downloads failed, so the fast path
  short-circuited forever and replicas never recovered. Failed staging files
  are deleted immediately.
- Manifest hashes are cached by `(size, mtime)`, so a ~2 GB sync tree no longer
  starves the async runtime on every manifest request.
- Loopback requests bypass the egress proxy (`localhost`, `127.0.0.1`, `::1`,
  plus operator `NO_PROXY`). `reqwest` proxied the agent's own `:8080` requests
  and broke file-server failover.
- The hub health probe timeout is configurable (default 10s), so a
  busy-but-healthy worker is not reported offline.

### Added

- Tests covering sync retry, file-server failover on both agent and hub sides,
  quota failover, alert throttling, and the `frpc` probe (zombie, self-match,
  foreign path, empty output). 58 tests total.

### Changed

- Docs state the installer variables must follow the pipe:
  `VM_ID=vm2 curl ... | bash` exports `VM_ID` to `curl` only, so the installer
  saw an empty `VM_ID`.

### Upgrade notes

Publish the new binary to the **VM-01** installer mirror only; replicas pick it
up through sync. Publishing directly to a replica is reverted, because sync is
a blind mirror of the master.
