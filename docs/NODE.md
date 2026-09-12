# alphanumeric — node reference

> The node's own documentation, kept as upstream wrote it. For the desktop wallet
> this repository adds, see [the README](../README.md).

<img width="862" height="696" alt="Screenshot 2026-07-27 at 8 18 50 AM" src="https://github.com/user-attachments/assets/24268ad5-2547-4c90-828f-c40242e490c5" />

[![Rust](https://img.shields.io/badge/Rust-stable-orange)](#build-from-source)
[![Platform](https://img.shields.io/badge/Platform-macOS%2FOSX%20%7C%20Linux%20%7C%20Windows-blue)](#supported-platforms)
[![License](https://img.shields.io/badge/License-MIT-green.svg)](#license)

https://www.alphanumeric.blue/

`alphanumeric` is a proof-of-work Layer 1 blockchain whose transactions are signed with
**ML-DSA-87 (FIPS 204)**, the NIST post-quantum lattice signature, rather than an elliptic
curve. This single binary is the full node, wallet and miner for macOS/OSX, Linux and Windows.

## At a Glance

| | |
|---|---|
| Ticker | `ALPHA` (glyph `♦`), 8 decimals, 1 ALPHA = 100,000,000 units |
| Consensus | Proof of work, ~5 s target block time |
| Signatures | ML-DSA-87 (FIPS 204): signature 4,627 B, public key 2,592 B |
| Addresses | 40 lowercase hex characters, `SHA256(public_key)[..20]` |
| Finality | Trusted checkpoint trailing the tip by 64 blocks |
| Storage | Embedded `redb` (pure-Rust, ACID), with a signed bootstrap snapshot for fast first sync |
| Default P2P port | `7177` |

## Quick Nav

**Run a node:** [Build from Source](#build-from-source) · [Bootstrap and Storage](#bootstrap-and-storage) · [Configuration](#configuration-via-environment-variables) · [CLI Surface](#cli-surface) · [Operations Checklist](#operations-checklist)

**Understand the chain:** [System Goals](#system-goals) · [Technical Architecture](#technical-architecture) · [Consensus and Validation](#consensus-and-validation) · [Tokenomics](#tokenomics) · [Security Posture](#security-posture)

**Build on it:**

| Document | What it covers |
|---|---|
| [`EXPLORER_API.md`](../EXPLORER_API.md) | Read API and transaction submit: endpoints, fees, finality, failure handling |
| [`SIGNING_SPEC.md`](../SIGNING_SPEC.md) | The exact signed-message format, encodings, and a deterministic test vector |
| [`docs/EXCHANGE_INTEGRATION.md`](EXCHANGE_INTEGRATION.md) | Asset identity, deposits, withdrawals, queue limits, node requirements |
| [`docs/GPU_MINING.md`](GPU_MINING.md) | GPU mining setup and tuning. This branch carries the GPU backend |
| [`docs/CONSENSUS_DECISIONS.md`](CONSENSUS_DECISIONS.md) | Why the consensus rules are what they are |
| [`docs/THREAT_MODEL.md`](THREAT_MODEL.md) | Threats considered and the controls against them |

## System Goals

`alphanumeric` is designed as a single-node executable that bundles the full operational stack needed to participate in a live network:

- deterministic local chain-state persistence (`redb`)
- bounded, framed P2P messaging with peer lifecycle management
- pool payouts with zero on-chain transactions via coinbase rotation (see `docs/POOL_PAYOUTS.md`)
- block/transaction propagation and sync workflows
- integrated mining path
- wallet/key workflows plus operator CLI
- local operational telemetry and diagnostics

## Non-Goals (Current)

- protocol stability guarantees across all commits
- audited production security claims
- strict long-term API/CLI compatibility guarantees

## Current Status

- Active development.
- Interfaces and internals can change between commits.
- Extensively reviewed and tested through internal adversarial and AI-assisted
  hardening. This is not a third-party audit or a guarantee that no defects remain.
- macOS/OSX release packaging is supported for the command-line client.

## Supported Platforms

The client is intended to run on:

- macOS/OSX, including Apple Silicon release builds
- Linux
- Windows

The repository can be built from source with the Rust stable toolchain. Prebuilt macOS/OSX
release archives are published on the [releases page](https://github.com/OSXBasedAnon/alphanumeric/releases).
Release zips may include a more user-focused `README.md` from `release/README.md`; this
repository README is the technical project overview.

## Technical Architecture

High-level module map:

- `src/main.rs`: process entrypoint, bootstrap, CLI loop, network command handling
- `src/a9/node.rs`: P2P runtime, framing, peer management, sync, event handling
- `src/a9/blockchain.rs`: block/transaction validation and persistence
- `src/a9/mgmt.rs`: wallet management and key workflow
- `src/a9/miner.rs`: mining manager and mining flow
- `src/a9/velocity.rs`: velocity/shred propagation support
- `src/a9/bpos.rs`: sentinel/validator-related logic
- `src/a9/whisper.rs`: whisper messaging support

Runtime shape:

1. bootstrap/load DB (`blockchain.db`)
2. initialize blockchain state
3. initialize node runtime + listeners
4. spawn background tasks:
   - peer maintenance
   - discovery/announce
   - sync
   - optional stats
5. process interactive commands and network events

## Network and Protocol Notes

- Default node TCP port: `7177` (`DEFAULT_PORT` in `src/a9/node.rs`)
- Outbound messaging uses framed transport (length-prefixed payloads)
- Message size limits are enforced (`MAX_MESSAGE_SIZE`)
- Outbound connection pooling is enabled with:
  - idle cleanup
  - LRU-style eviction
  - per-peer circuit breaker on repeated failures
- Inbound connection handling is concurrency-limited
- DNS/discovery endpoints are environment-configurable
  - Primary peer bootstrap: `ALPHANUMERIC_DISCOVERY_BASE` (default `https://alphanumeric.blue`)
  - Optional DNS fallback seeds: `ALPHANUMERIC_DNS_SEEDS` (comma-separated `host:port`)

## Consensus and Validation

The codebase includes multiple consensus/validation-related components (PoW/mining path, sentinel/validator logic, and propagation optimizations). Behavior is defined by the current code paths in `src/a9/*`.

Transaction witnesses use a compact-finality model:

- live mempool and new block admission require the full ML-DSA signature and sender public key
- confirmed block storage keeps a compact signature receipt plus `sig_hash = SHA256(full_signature)`
- historical P2P sync validates block hash, merkle root, PoW, balances, reward rules, public-key/address binding, and receipt commitments without requiring archived full witnesses

Difficulty maps to PoW work in discrete power-of-two bands (the target is
`MAX_TARGET >> (difficulty / 16)`), so the retarget adjusts real work in factor-of-two
steps rather than continuously. At hashrates that fall between two bands, observed block
time can sawtooth around the `TARGET_BLOCK_TIME` (faster in the lower band, slower in the
higher one) until difficulty or hashrate settles. This is expected and self-correcting —
it does not affect finality (reorgs remain bounded by the checkpoint margin) — and finer
target granularity is a candidate for a future coordinated protocol upgrade.

If you are integrating against this repository, build from a **release tag**, not from `main`.
`main` carries work that has not shipped, so behaviour observed there may not match any
binary on the network. Pinning an arbitrary commit is worse still: a commit that predates a
consensus activation will disagree with the network once the chain reaches that height.

### Consensus activations

Consensus rules change at scheduled block heights, compiled into the client rather than
signalled at runtime: a node compares the block index against the activation constant and
switches by itself, with no configuration, restart or operator action at the boundary.

The practical consequence is that **an operator must be on a release that contains an
activation before the chain reaches it.** Software that predates one computes different
values from that height on, disagrees with the network about block validity, and follows a
chain the rest of the network has abandoned. The node announces an advisory consensus
fingerprint so operators can monitor rollout compatibility.

Which heights are pending, and the minimum release for each, are listed in the
[release notes](https://github.com/OSXBasedAnon/alphanumeric/releases) for the current
version. Run the current release and this takes care of itself.

## Tokenomics

### Supply Summary (Simple)

- There is **no fixed hard cap** encoded as a single number.
- New issuance **decays over time**:
  - max block reward drops by **17% every 6 months** (`* 0.83` each period)
- In practice this creates **asymptotic supply behavior**:
  - total supply can continue to increase
  - but new issuance becomes progressively smaller over time
- Launch genesis is dated **2026-07-04 UTC**. Any forward supply projection must
  state its starting height/time, assumed block cadence, and transaction-fee
  activity; an undated “max supply from now” estimate is not authoritative.

### Runtime Parameters (Current Code)

- Reference-wallet fee: automatic, priced off the live mempool
  (`Blockchain::fee_estimate`). The relay floor is `0.0001`. Full policy, including the
  explicit `--fee` ceiling, is under [CLI Surface](#cli-surface)
- `FEE_PERCENTAGE = 0.000563063063` remains the Whisper encoding constant; it is
  not the regular-wallet fee policy
- Reward constants: `MIN_BLOCK_REWARD = 1.0`, launch
  `MAX_BLOCK_REWARD = 50.0`; the effective subsidy ceiling decays by 17% every
  six months and eventually falls below the nominal floor
- Reward network fee: `NETWORK_FEE = 0.0005`, the pinned fee on the coinbase transaction
- Target block time: `TARGET_BLOCK_TIME = 5` seconds
- Empty-block rewards are clamped from `0.2 * current_max` into
  `[min(MIN_BLOCK_REWARD, current_max), current_max]`
- Two reward curves exist, selected by block height at the Reward Curve V2 activation.
  Below it, the legacy curve damps the fee contribution by `MINT_CLIP = 0.35` and is frozen
  permanently, because changing its operation order would invalidate historical coinbases.
  At and above it, miner compensation is the scheduled subsidy plus 65% of included
  transaction fees, with the remaining 35% burned: the decaying ceiling bounds the subsidy
  component, and exact fee units are transferred separately in integer arithmetic. See
  [`docs/CONSENSUS_DECISIONS.md`](CONSENSUS_DECISIONS.md)

Actual realized issuance still depends on real network activity (block production + transaction fees).

## Build from Source

Prerequisites:

- Rust stable toolchain
- Cargo
- macOS/OSX: Xcode Command Line Tools (`xcode-select --install`) if a local compiler toolchain is missing

Build:

```bash
cargo build --release
```

Run:

```bash
cargo run --release
```

### Mining on GPU

This branch mines on **CPU only**. The GPU backend is not a build flag or a runtime option
here: it lives on the [`gpu-mining`](https://github.com/OSXBasedAnon/alphanumeric/tree/gpu-mining)
branch, which carries the wgpu compute kernel and the extra dependencies that go with it.

```bash
git checkout gpu-mining
cargo build --release --features gpu_miner
```

That build then selects the backend at runtime with `mine <wallet> --gpu` or `--cpu`. A build
without the `gpu_miner` feature defaults to CPU and refuses `--gpu`. Setup and tuning are in
[`docs/GPU_MINING.md`](https://github.com/OSXBasedAnon/alphanumeric/blob/gpu-mining/docs/GPU_MINING.md)
on that branch.

Both branches mine the same chain under the same consensus rules. `gpu-mining` adds the GPU
miner on top of the node; it is maintained alongside `main` rather than merged from it, so
take releases from the tags rather than assuming the two branches are identical.

Run the built binary directly:

```bash
./target/release/alphanumeric
```

For a cleaner local install, keep the binary in a dedicated folder and always run it from that folder, or set `ALPHANUMERIC_DB_PATH` explicitly.

## Bootstrap and Storage

Startup bootstrap source (default):

- The signed manifest at `https://alphanumeric.blue/api/bootstrap/manifest`; the snapshot download URL is taken from that signed manifest (there is no fixed static download path).

Bootstrap trust mode:

- Nodes prefer manifest bootstrap from `https://alphanumeric.blue/api/bootstrap/manifest`.
- The manifest is signature-verified before use.
- If manifest retrieval/parsing/verification fails, startup fails closed by default.
- Bootstrap is manifest-verified and fails closed on verification failure; there is no override to bypass verification.

Launch-network guard:

- `blockchain.db` is reused only when block `0` matches the frozen launch genesis/network ID.
- If a local DB belongs to a different network, has a bad genesis, or cannot be read, startup replaces it from the signed bootstrap.
- Wallet keys are separate from chain state; keeping `private.key` preserves wallet identity, but balances are always calculated from the verified launch-chain DB.
- `ALPHANUMERIC_FORCE_BOOTSTRAP=true` forces replacement from the signed bootstrap even when the local DB is already valid.

Default storage behavior:

- `ALPHANUMERIC_DB_PATH` controls the chain database path.
- Without `ALPHANUMERIC_DB_PATH`, the default relative path is `blockchain.db`.
- Relative paths resolve under the current working directory, unless an existing launch-network DB or stale DB is found beside the executable and needs to be reused/replaced.
- For normal users, a dedicated folder such as `~/Alphanumeric` is recommended.

Primary local artifacts:

- `blockchain.db`
- `private.key`
- `node_identity.key`
- optional lock files (`*.lock`)

Wallet key file invariants (`private.key`):

- Two records may not share a wallet name. This is checked when the file is LOADED, so a file
  with duplicate names fails at startup.
- Two records may not share a wallet address. This is checked only when the file is WRITTEN.
  Builds before the seed bridge did not check it at all, so an existing `private.key` may
  already contain two records with the same address: it still loads and its wallets still
  spend, but every command that rewrites the file — `new`, `rename`, `import-seed` — refuses
  with a message naming the duplicate address. Delete the redundant record (both hold the same
  key, so nothing is lost) and those commands work again. Rewriting such a file is what would
  make it permanently ambiguous which record signs for that address, which is why the refusal
  is at the write and not the read.

## Configuration via Environment Variables

Common variables used by the runtime include:

- `ALPHANUMERIC_BIND_IP`
- `ALPHANUMERIC_PORT` (P2P listen port; defaults to `7177`)
- `ALPHANUMERIC_DB_PATH`
- `ALPHANUMERIC_EXPLORER_API` (opt-in HTTP read API plus transaction submit. Accepts a bare
  port, bound to loopback, or `host:port`. Off unless set; this is what an integration or a
  block explorer talks to. See [`EXPLORER_API.md`](../EXPLORER_API.md))
- `ALPHANUMERIC_BLOCKNOTIFY` (runs a command on every new chain tip, following Bitcoin
  Core's `-blocknotify` contract: `%s` is the block hash, `%h` the height. Useful for pools
  and deposit scanners that would otherwise poll; legacy whitespace-delimited syntax)
- `ALPHANUMERIC_BLOCKNOTIFY_ARGV` (preferred when a program path or argument contains
  spaces: a JSON string array such as `["C:\\Program Files\\Pool\\notify.exe","%s","%h"]`;
  takes precedence over `ALPHANUMERIC_BLOCKNOTIFY` and is executed directly without a shell)
- `ALPHANUMERIC_HEADLESS` (`true` runs node services without the interactive command loop.
  See "Headless mining" below)
- `ALPHANUMERIC_MINE` (headless only: a wallet name or address to mine to, continuously.
  Unset means the headless node runs services and does not mine)
- `ALPHANUMERIC_MINE_BACKEND` (headless only: `gpu` or `cpu`; defaults to whatever the
  binary was built for. Any other value, or `gpu` on a binary without GPU support, is
  refused before the database opens rather than silently downgraded)
- `ALPHANUMERIC_FORCE_BOOTSTRAP`
- `ALPHANUMERIC_IGNORE_DB_LOCK`
- `ALPHANUMERIC_STATS_ENABLED`
- `ALPHANUMERIC_STATS_BIND` (default `127.0.0.1`; set `0.0.0.0` only when the stats API should be public)
- `ALPHANUMERIC_STATS_PORT`
- `ALPHANUMERIC_SEED_NODES` or `ALPHANUMERIC_BOOTSTRAP_PEERS` (comma-separated `host:port` peers tried before relying on gateway fallback)
- `ALPHANUMERIC_DNS_SEEDS`
- `ALPHANUMERIC_DISCOVERY_BASE`
- `ALPHANUMERIC_DISCOVERY_BASES`
- `ALPHANUMERIC_ALLOW_PRIVATE_PEERS` (default off; use only for local/private test networks)
- `ALPHANUMERIC_DISCOVERY_URL`
- `ALPHANUMERIC_ANNOUNCE_URL`
- `ALPHANUMERIC_HEADERS_URL`
- `ALPHANUMERIC_ANNOUNCE_INTERVAL_SECS` (default `300`, minimum `60`)
- `ALPHANUMERIC_ENABLE_HEADER_SNAPSHOTS` (default off; enable on trusted publisher/validator nodes only)
- `ALPHANUMERIC_HEADER_SNAPSHOT_INTERVAL_SECS` (default `30`, minimum `15`, maximum `3600`)
- `ALPHANUMERIC_ENABLE_STATS_SNAPSHOTS` (default off; enable on trusted publisher/validator nodes only)
- `ALPHANUMERIC_STATS_SNAPSHOT_INTERVAL_SECS` (default `300`, minimum `60`)
- Relay publishing and relay sync are **always on** and have no toggle. They are how a node
  reaches the chain when direct peers are unreachable, so they are not opt-in.
- `ALPHANUMERIC_RELAY_SYNC_BACKFILL_DEPTH` (default `64`, the checkpoint reorg margin; max `256`)
- `ALPHANUMERIC_RELAY_SYNC_MAX_ROUNDS` (default `4`, max `24`)
- `ALPHANUMERIC_PUBLIC_IP`
- `ALPHANUMERIC_PEER_CACHE_PATH`
- `ALPHANUMERIC_TX_WITNESS_CACHE_SIZE`
- `ALPHANUMERIC_OUTBOUND_SCHEDULER` (default `true`; transport-only rollback switch for bounded,
  class-aware TCP egress. Setting `false` restores the legacy direct/message-count path; it does
  not change consensus, wire messages, or stored data)
- `ALPHANUMERIC_OUTBOUND_GLOBAL_QUEUE_MIB` (default `64`, accepted range `32..=512`; encoded bytes
  waiting or in flight across all authenticated TCP peers)
- `ALPHANUMERIC_OUTBOUND_PEER_QUEUE_MIB` (default `16`, accepted range `16..=64`; encoded bytes
  waiting or in flight for one peer; class sublimits reserve room for block/control traffic)
- `ALPHANUMERIC_OUTBOUND_MAX_QUEUE_AGE_MS` (default `10000`, accepted range `1000..=60000`;
  transaction/control classes use half this value, with a one-second minimum)
- `ALPHANUMERIC_OUTBOUND_TX_MIB_PER_SEC` (default `16`, accepted range `1..=256`; per-peer
  transaction byte-token refill rate, with a two-second burst)
- `ALPHANUMERIC_OUTBOUND_TX_WORK_PER_SEC` (default `4096`, accepted range `256..=65536`; per-peer
  transaction work-token refill rate, where batched bodies are charged per transaction)

The opt-in `/stats` response includes `outbound_relay` gauges and counters for each traffic class:
queued/in-flight bytes, queue wait, completion/failure, expiry, rate rejection, and capacity
rejection. Per-peer scheduler state is independently capped at 4,096 entries even if the configured
peer limit is unreasonable. Optional WebRTC transaction copies have their own 8 MiB/256-task
nonblocking lane and drop counter, so mesh saturation cannot consume canonical TCP capacity; block
mesh traffic remains separate and valid-PoW bounded. These are local transport-policy observations
and never affect consensus or checkpoint advancement.

Official bootstrap snapshots are accepted only when the blue gateway returns a pinned publisher manifest with a valid signature and SHA-256. New manifests also carry signed compressed size, extracted size, and file count metadata so the node can preflight disk space and verify extraction without imposing a fixed chain-size ceiling.

Bootstrap publishing is maintainer infrastructure, not part of normal macOS node setup. Operator-level details are kept in [docs/BOOTSTRAP_PUBLISHER.md](BOOTSTRAP_PUBLISHER.md).

### Headless mining

A node started with `ALPHANUMERIC_HEADLESS=1` runs its services and nothing else — the
default, and still the right choice for a node that only relays and answers `/explorer`.
To make it mine, name a wallet:

```
ALPHANUMERIC_HEADLESS=1 ALPHANUMERIC_MINE=my_wallet ./alphanumeric
```

`ALPHANUMERIC_MINE` takes a wallet **name or address**. A name that is not a loaded
wallet stops startup with an error that lists what *was* loaded. This check runs after
wallets are loaded, which means after the database has opened (and, on a node with no
local chain data yet, after a bootstrap snapshot has been fetched) — a typo here is not
free. If `private.key` is absent from the working directory entirely, the error says so
and names the directory, ahead of the general encrypted-wallet caveat below — a systemd
unit with the wrong `WorkingDirectory=` is a wrong-directory problem, not a passphrase
problem.

`ALPHANUMERIC_MINE_BACKEND` is `gpu` or `cpu`; the default is whatever the binary was
built for (this repository's default build is CPU-only — GPU support is an opt-in
Cargo feature). Asking for `gpu` on a binary built without GPU support, or any value
that is neither, is refused rather than downgraded — a silent downgrade mines at a
fraction of the expected rate and the notice scrolls away. Unlike the wallet-name check
above, this one is cheap: it runs immediately after the environment variables are
parsed, before the database opens or anything is fetched, so a bad backend value fails
instantly with nothing created in the working directory.

Headless mining is always continuous: there is no operator to restart it after one
block.

**The mining wallet must be unencrypted.** Headless cannot prompt for a passphrase, so
encrypted wallets are skipped at load. Naming one — or naming anything when no
unencrypted wallet exists at all — stops startup rather than yielding a node that was
told to mine and does not.

**Headless with no `ALPHANUMERIC_MINE` set is unchanged**: it prints `Headless mode
enabled. Node services are running.` and keeps running, exactly as it did before this
mining option existed.

**`ALPHANUMERIC_MINE` without `ALPHANUMERIC_HEADLESS` is refused**, not ignored. Mining
from an environment variable happens only in headless mode, so a node handed the one
without the other would start the interactive menu and mine nothing — silently, for as
long as nobody looked. Startup stops and says to add `ALPHANUMERIC_HEADLESS=1` or to
unset the variable and use the `mine` command instead.

`/explorer/status` reports what it is mining — see `EXPLORER_API.md`. Note that
`mining_address` is the wallet the session mines **for**: with
`ALPHANUMERIC_COINBASE_PAYOUTS` set, the coinbase pays a rotating address from that
schedule instead, and `mining_payout_rotation` in the same response says so.

## CLI Surface

Interactive command loop examples:

- `create <sender> <recipient> <amount> [--fee <ALPHA>]`
- `whisper <address> <msg>` (amount can be provided depending on flow)
- `balance`
- `new [wallet_name]`
- `account [address_or_wallet_name]` (bare: your default wallet)
- `history`
- `rename <old_name> <new_name>`
- `export-seed <wallet_name>` (prints the wallet's 32-byte ML-DSA seed as 64 hex characters
  after a confirmation, and after re-entering the passphrase for an encrypted wallet. That
  seed is the whole backup and anyone who reads it owns the funds.)
- `import-seed [hex64] [name]` (rebuilds a wallet from such a seed. **Run it with no
  arguments**: it then asks for the seed at a masked prompt, which is never echoed and never
  reaches the command history. The positional form is kept for scripts — `SIGNING_SPEC.md`
  documents it — but a seed typed as an argument is echoed to the screen as you type it and
  stays in your terminal's scrollback, and the line editor keeps its own copy that the node
  cannot reach. The node skips its command history and wipes every copy it owns for any line
  carrying something seed-shaped — not just for the correctly spelled command — but those two
  are outside it. The masked prompt has the same caveat in a smaller form: the prompt library
  holds the input in a buffer of its own, as it does for the startup passphrase and
  `export-seed`.)
- `mine [wallet_name] [--continuous|-c] [--gpu|--cpu]` (bare: rewards go to your default
  wallet). GPU mining requires building this branch with `--features gpu_miner`; the backend
  is then chosen at runtime with `--gpu` or `--cpu`. A build without that feature defaults to
  CPU and refuses `--gpu`. See [`docs/GPU_MINING.md`](GPU_MINING.md)
- `info`
- `debug`

With no `--fee`, the wallet prices the fee automatically off the live mempool
(the `info` screen shows the current value as `Default Fee`, and `create`
prints the resolved `Auto fee` before signing): a flat `0.0002` anchor when the
network is quiet, one unit above the marginal next-block fee under congestion,
never above `0.002` for an automatic fee. Exchanges and other
automated operators can select an absolute fee with `--fee`; values must meet
the `0.0001` relay floor. The CLI refuses an explicit fee above `0.01` as a
hard safety ceiling. This is reference-wallet policy, not a universal network
limit; externally signed integrations retain control of their fee policy
subject to current node admission and block-accounting rules (integrators can
query `GET /explorer/fee-estimate` for the same recommendation).

Network commands (at the REPL prompt):

- `--status`
- `--sync`
- `--connect <ip:port>`
- `--getpeers`
- `--discover`

## Security Posture

Alphanumeric is post-quantum settlement infrastructure. Its security model is built for the
exchanges, custodians, pools, and validators that hold value on the chain — not only for a single
operator running one node.

### Cryptography and consensus

- **Post-quantum authentication.** Every transaction is signed with ML-DSA-87 (FIPS 204), the NIST
  lattice signature, with sender-to-key binding enforced during validation. There is no
  elliptic-curve signature to migrate away from.
- **Deterministic, integer-exact validation.** Block validity, difficulty, fee accounting, and
  reward economics are evaluated with checked integer arithmetic and are identical across nodes; no
  floating-point tolerance band is applied to any consensus check.
- **Deterministic finality.** A trusted checkpoint is placed a fixed reorg margin behind the
  tip each time it advances, and it advances deterministically — never accelerated, paused, or
  lowered by network-health heuristics. It moves only when a block received from the network
  extends this node's tip or when the node converges with a signed beacon, so the gap between
  tip and checkpoint is that margin plus everything mined since; on a node mining a large share
  of the chain it is routinely much wider than the margin. See `EXPLORER_API.md`.
- **Replay and freshness protection.** Transactions carry a bounded freshness window backed by a
  replay registry, so a confirmed payment cannot be re-mined and the registry stays bounded.
- **Verified bootstrap only.** Fast-sync snapshots are accepted solely under a pinned publisher
  signature and SHA-256, with signed size and file-count bounds and streamed verification. There is
  no unverified fallback, and no environment variable can relax those bounds.

### Network and denial-of-service resistance

- All peer input is untrusted and processed under bounded, length-framed messaging with per-peer and
  per-class byte/work limits, rate limiting, and authenticated encrypted sessions.
- Admission and validation are **fail-closed**: malformed, oversized, ambiguous, or
  unverifiable input is rejected rather than best-guessed.

### Integrating securely (exchanges, custodians, pools)

- **Key custody stays with you.** The node never requires custody of your signing keys. Sign in your
  own HSM or keystore and submit the finished transaction; the submission API accepts pre-signed
  transactions only.
- **Do not expose the node directly.** The JSON API has no authentication of its own by design. Bind
  it to loopback and place it behind your own authenticated reverse proxy — that trust boundary is
  yours to own.
- **Idempotent, collision-safe withdrawals.** Submit through the protected endpoints
  (`/explorer/v2/submit-tx`) with a unique, unguessable idempotency key per withdrawal. The node
  then makes retries safe and detects same-parameter payment collisions before they can double-pay.
  See [`docs/EXCHANGE_INTEGRATION.md`](EXCHANGE_INTEGRATION.md).

### Assurance and disclosure

- The consensus, networking, storage, and wallet paths are under a continuous adversarial review
  program: property and boundary tests, fault injection, differential checks against the reference
  paths, and threat-model-mapped controls documented in
  [`docs/THREAT_MODEL.md`](THREAT_MODEL.md). Every fixed security issue is pinned by a
  permanent regression test.
- This review is internal; an independent third-party audit is a roadmap item and is not yet
  complete. We state that plainly rather than imply coverage we do not have.
- **Reporting a vulnerability.** Report suspected vulnerabilities **privately** — do not open public
  issues or post in Discord for security matters. Use this repository's private vulnerability
  reporting (GitHub → **Security → Report a vulnerability**). We practice coordinated disclosure and
  will acknowledge a valid report promptly and agree remediation and disclosure timelines with the
  reporter.

## Operations Checklist

Minimum recommended setup for a reachable node:

1. open TCP port `7177` on host firewall/router
2. run node on a stable host with persistent disk
3. monitor logs and peer count
4. back up sensitive key material securely

Windows firewall example:

```powershell
New-NetFirewallRule -Name "Alphanumeric Inbound" -DisplayName "Alphanumeric Network (Port 7177 in)" -Protocol TCP -LocalPort 7177 -Direction Inbound -Action Allow
New-NetFirewallRule -Name "Alphanumeric Outbound" -DisplayName "Alphanumeric Network (Port 7177 out)" -Protocol TCP -RemotePort 7177 -Direction Outbound -Action Allow
```

macOS/OSX firewall note:

- If the macOS firewall prompts for incoming connections, allow `alphanumeric` if this machine should accept peers.
- If Gatekeeper blocks a downloaded release binary, right-click the binary in Finder and choose Open, or remove the quarantine attribute with `xattr -dr com.apple.quarantine ./alphanumeric`.

## Development Workflow

Quick local checks:

```bash
cargo check
```

When changing protocol/runtime code, prefer:

- explicit message framing
- bounded buffers and timeouts
- clear lock scopes
- deterministic error handling

Threat model and control mapping:

- `docs/THREAT_MODEL.md`

## Frontend

- Official frontend: https://www.alphanumeric.blue/

## Community

- Discord: https://discord.gg/D3r7TRcj9t

## License

MIT
