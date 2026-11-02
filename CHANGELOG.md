# Changelog

All notable changes to this project are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning is [SemVer](https://semver.org/).

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
