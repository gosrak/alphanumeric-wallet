# GPU Mining

The GPU miner is an **opt-in build** of the alphanumeric client. It hashes the
BLAKE3 proof-of-work on the GPU via [wgpu](https://wgpu.rs) — using **Vulkan,
DirectX 12, or Metal**, whichever your platform provides. There is **no CUDA and
no vendor SDK** to install.

GPU mining is a *performance* path only. Every block a GPU finds still goes
through the full CPU-side validation and the network's consensus checks before
it counts — mining is not consensus.

---

## 1. Build

The GPU miner lives behind the `gpu_miner` cargo feature (off in the stock
client). Build it with:

```sh
cargo build --release --features gpu_miner
```

`webrtc_mesh` is on by default, so the command above is a complete miner. If you
also run a bootstrap publisher, add it: `--features gpu_miner,bootstrap_publisher`.

A stock (CPU-only) build has **no** `--gpu` flag and will refuse it.

---

## 2. Run

```
mine <miner_wallet_name> [--continuous] [--gpu|--cpu]
```

- In a **GPU build, `--gpu` is the default** — `mine <wallet>` already mines on
  the GPU. `--cpu` forces CPU on a GPU build.
- `--continuous` keeps mining across blocks (paced to the network tip) instead
  of stopping after one.
- On start the miner prints the adapter and backend it selected, e.g.
  `NVIDIA GeForce RTX 4090 (Vulkan)`, or the reason it fell back to CPU.
- Before it mines, it runs a **self-check**: it hashes a known header on the GPU
  and compares to the CPU `blake3` reference. If they disagree — or GPU init
  fails — it **falls back to CPU mining so the command never silently stalls**.

Press `Enter` (or Ctrl-C) to stop.

---

## 3. Backends & troubleshooting

The miner does **not** pick one backend and give up. It enumerates every backend
your system offers, drops CPU/software adapters so it can never silently "mine"
on llvmpipe, and tries the candidates in order — preferring the same card on
another backend (a broken Vulkan discrete GPU falls back to that same GPU on
DirectX 12) over a weak integrated one that could be slower than the CPU miner.

So forcing a backend is **not** the first thing to try, and it can make things
worse: `WGPU_BACKEND` **restricts** the miner to that one backend, removing the
automatic fallback it would otherwise have used. Set it only to pin a specific
backend for diagnosis:

| Shell | Command (before launching the miner, in the same session) |
|---|---|
| Windows CMD | `set WGPU_BACKEND=dx12` |
| Windows PowerShell | `$env:WGPU_BACKEND="dx12"` |
| Linux / macOS | `WGPU_BACKEND=dx12 ./alphanumeric` |

Valid values: `vulkan`, `dx12`, `gl`, `metal`. Unset it again to restore the
automatic fallback.

Common cases:

- **`enumerate_adaptors: initialization of an object has failed`** — no usable
  adapter was found on ANY backend, so this is a driver or visibility problem,
  not a backend choice. Forcing `WGPU_BACKEND` will not help and usually hurts.
  Check the cases below, and confirm the GPU is visible to the OS at all
  (`nvidia-smi` on NVIDIA; if that cannot talk to the driver, nothing will).
- **NVIDIA CMP / mining cards** (e.g. CMP 30HX/40HX/…): their Vulkan is usually
  crippled, and the miner already falls back to DirectX 12 on the same card by
  itself. If it still fails, the driver is the problem, not the backend.
- **Remote Desktop (RDP)** hides the physical GPU, so nothing enumerates. Use a
  real monitor or a **dummy HDMI/DP plug**, or a remote tool that doesn't hijack
  the GPU (AnyDesk / Parsec), or run the miner as a service.
- **If no backend works**, the miner mines on **CPU** so you're never stuck; the
  status line will say it fell back.

---

## 4. Multi-GPU

One process mines on **every usable GPU at once** (since 8.1.0). Each card gets
its own worker thread and its own slice of the nonce space; the first card to
find a block wins the attempt and the others stop. Nothing needs configuring
for that.

To limit the set, name the cards by their 0-based index:

```sh
ALPHANUMERIC_GPU_DEVICES=0,2 ./alphanumeric    # cards 0 and 2, not 1
ALPHANUMERIC_GPU_INDEX=1 ./alphanumeric        # exactly card 1 (the older switch, still honoured)
```

Windows: `set ALPHANUMERIC_GPU_DEVICES=0,2` in the CMD session that starts the
node, or the same variable in the service definition.

**Mapping index → card:** the stats endpoint (`/stats` on the stats port) always
carries `gpu_devices`, one `{index, name}` per usable card, whether or not
anything is mining; the miner also prints the roster at start:

```
  GPU [0] NVIDIA GeForce RTX 4090 (Vulkan)
  GPU [1] NVIDIA GeForce RTX 3090 (Vulkan)
  GPU [2] NVIDIA CMP 30HX (Dx12)
```

A card that appears under two backends (Vulkan and GL on Linux, DX12 too on
Windows) is listed once, under the first backend found. An out-of-range index
is reported (e.g. `GPU index 5 is out of range: 3 usable GPU(s) found (valid
0..=2)`).

**Per-card figures.** While mining, `/stats` adds `mining_devices`: one entry
per card with its hashrate, hashes this session and, on NVIDIA cards with the
driver's NVML available (`nvml.dll` on Windows, `libnvidia-ml.so.1` elsewhere),
the core and memory clocks, temperature and power draw. Other vendors show
`null` there. `mining_hashes`, `mining_difficulty`, `mining_expected_block_secs`
and `mining_threads` (CPU backend) sit beside it.

**A card that dies** mid-session (driver reset, hang) drops out of the pool and
the rest carry on; when the last one dies the session falls back to CPU mining,
as before.

`WGPU_BACKEND` still applies to the whole process; set it when a particular rig
needs one backend for every card.

---

## 5. Reading the output

Each process shows a live status line with:

- **Hashrate** (GH/s) for that card,
- the **difficulty** of the block it is currently mining, and
- an **ETA** to the next block at the current rate.

Note the difficulty shown is the **next block's** target (what the miner is
working toward), which can differ slightly from the sealed-tip difficulty an
explorer shows — the miner leads the chain tip by one block's retarget.

---

## 6. Notes

- GPU mining requires the `gpu_miner` feature **at build time**; you cannot
  enable it at runtime on a stock client.
- The self-check + full block validation guarantee correctness regardless of
  backend — a GPU that computes a wrong hash is rejected and demoted to CPU, it
  never produces bad blocks.
- Mining GPU vs CPU is purely a speed choice; both submit blocks through the same
  validation and gossip path.
