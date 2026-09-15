//! GPU mining backend (feature `gpu_miner`, opt-in at runtime).
//!
//! Searches the 92-byte header nonce space on the GPU via a WGSL BLAKE3 kernel
//! (wgpu: Metal / Vulkan / DX12 — no system deps). Mining is NOT consensus: a
//! wrong hash here can only waste the local GPU's time, because every produced
//! block still goes through the full CPU-side validation and the network's
//! rules. Correctness is nevertheless locked by tests that compare the kernel's
//! hash byte-for-byte against the `blake3` crate.

use std::sync::atomic::{AtomicBool, Ordering};

use bytemuck::Zeroable;

const WGSL: &str = include_str!("gpu_blake3.wgsl");
const WORKGROUP: u32 = 256;
/// Sentinel zero_bits value: kernel thread 0 writes its raw hash to the result
/// buffer instead of searching (test/self-check mode).
const DEBUG_HASH_SENTINEL: u32 = 0xFFFF_FFFF;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    header: [u32; 24], // w0..w23 as 6 x vec4<u32>
    nonce_lo: u32,
    nonce_hi: u32,
    zero_bits: u32,
    threads: u32,
    iters: u32,
    // Pad the whole struct to 128 bytes: a WGSL uniform struct is 16-byte
    // aligned, so the shader-side size rounds 120 -> 128 and the binding must
    // match (wgpu rejects a 120-byte buffer as < minimum 128).
    _pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ResultBuf {
    found: u32,
    nonce_lo: u32,
    nonce_hi: u32,
    _pad: u32,
    hash: [u32; 8],
}

pub struct GpuMiner {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    params_buf: wgpu::Buffer,
    result_buf: wgpu::Buffer,
    readback_buf: wgpu::Buffer,
    // Buffer identities never change, so one bind group serves every dispatch
    // (rebuilding it per dispatch was pure per-dispatch churn).
    bind: wgpu::BindGroup,
    pub adapter_name: String,
}

impl GpuMiner {
    /// Initialize on the best available adapter. Errors are descriptive so the
    /// caller can fall back to CPU mining with a clear message.
    pub fn new() -> Result<Self, String> {
        pollster::block_on(Self::new_async())
    }

    async fn new_async() -> Result<Self, String> {
        // Identical to Instance::default() (all backends; Vulkan wins adapter
        // selection on NVIDIA) EXCEPT it honors WGPU_BACKEND — wgpu 22.1.0's
        // Instance::default() ignores the env var, which left no way to steer
        // a box with a broken driver stack (e.g. WGPU_BACKEND=dx12) without a
        // rebuild.
        let backends = wgpu::util::backend_bits_from_env().unwrap_or(wgpu::Backends::all());
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });
        // Multi-GPU (one process per card): ALPHANUMERIC_GPU_INDEX=N pins THIS
        // process to the Nth enumerated adapter, so a 3-GPU rig runs three
        // processes (index 0/1/2) and each card grinds a disjoint region of the
        // nonce space — the per-attempt RANDOM nonce base (see gpu_mine_attempt)
        // makes separate processes non-overlapping with no cross-process
        // coordination. Unset = auto-select (best adapter, with fallback). When
        // an index is set the full roster is printed first so an operator can
        // map index -> card.
        let gpu_index = std::env::var("ALPHANUMERIC_GPU_INDEX")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok());

        // The ordered list of adapters to try. Each is attempted (device init +
        // BLAKE3 self-check) in turn; the FIRST that passes is used.
        let candidates: Vec<wgpu::Adapter> = if let Some(idx) = gpu_index {
            // Explicit operator pin: use ONLY the chosen adapter, never fall
            // back — a sibling process is pinned to each other card, so falling
            // back here would double up one card and leave another idle.
            let adapters = instance.enumerate_adapters(backends);
            for (i, a) in adapters.iter().enumerate() {
                let gi = a.get_info();
                // device_type (DiscreteGpu/IntegratedGpu/…) so an operator can
                // tell at a glance which index is the real card vs the iGPU on a
                // mixed-GPU laptop/rig before pinning a process to it.
                eprintln!(
                    "  GPU [{}] {} ({:?}, {:?})",
                    i, gi.name, gi.backend, gi.device_type
                );
            }
            let n = adapters.len();
            let chosen = adapters.into_iter().nth(idx).ok_or_else(|| format!(
                "ALPHANUMERIC_GPU_INDEX={idx} is out of range: {n} GPU adapter(s) found (valid indices 0..={})",
                n.saturating_sub(1)
            ))?;
            vec![chosen]
        } else {
            // Auto-select with fallback: the best (HighPerformance) adapter
            // first, then every OTHER enumerated non-software adapter. A box
            // whose preferred backend is present but BROKEN — classically a bad
            // Vulkan driver on Windows/NVIDIA, where request_adapter still hands
            // back a Vulkan adapter that then fails device-init or the self-check
            // — now falls back to DX12 instead of silently demoting to CPU (the
            // exact silent-failure this backend otherwise works hard to avoid).
            // Cpu/software adapters (llvmpipe) are skipped so we never "succeed"
            // onto a path slower than the multicore CPU miner. WGPU_BACKEND is
            // honored: `backends` is already env-filtered, so an operator who
            // pinned a backend gets only that backend's adapters here — the
            // fallback never leaves their chosen backend set.
            let mut candidates = Vec::new();
            if let Some(primary) = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
            {
                // Skip a software primary (llvmpipe) too: request_adapter falls
                // back to software only when NO hardware GPU exists, and the
                // multicore CPU miner beats llvmpipe — so an empty candidate
                // list here cleanly demotes to CPU rather than "GPU-mining"
                // slower than the CPU path.
                if primary.get_info().device_type != wgpu::DeviceType::Cpu {
                    candidates.push(primary);
                }
            }
            for a in instance.enumerate_adapters(backends) {
                let gi = a.get_info();
                if gi.device_type == wgpu::DeviceType::Cpu {
                    continue; // never silently mine on llvmpipe
                }
                if candidates
                    .iter()
                    .any(|c| Self::same_adapter(&c.get_info(), &gi))
                {
                    continue; // already queued (the HighPerformance pick)
                }
                candidates.push(a);
            }
            // Prefer stronger fallbacks: keep the primary (the HighPerformance
            // pick) first, then order the rest DiscreteGpu-before-IntegratedGpu
            // so a broken discrete primary falls back to the SAME card on another
            // backend (e.g. DX12) rather than to a weak iGPU that could be slower
            // than the multicore CPU miner. Stable sort keeps enumerate order
            // within a device_type; len>1 guards the [1..] slice.
            if candidates.len() > 1 {
                candidates[1..].sort_by_key(|a| match a.get_info().device_type {
                    wgpu::DeviceType::DiscreteGpu => 0u8,
                    wgpu::DeviceType::IntegratedGpu => 1,
                    wgpu::DeviceType::VirtualGpu => 2,
                    _ => 3, // Other (Cpu already filtered out above)
                });
            }
            candidates
        };

        if candidates.is_empty() {
            return Err("no GPU adapter found (wgpu)".into());
        }

        // Try each candidate in order; the first that both initializes a device
        // AND passes the BLAKE3 self-check wins. Errors are collected so a total
        // failure reports WHY each adapter was rejected, not just "no GPU".
        let mut errors: Vec<String> = Vec::new();
        for adapter in candidates {
            let gi = adapter.get_info();
            let label = format!("{} ({:?})", gi.name, gi.backend);
            match Self::try_build(&adapter).await {
                Ok(miner) => return Ok(miner),
                Err(e) => errors.push(format!("{label}: {e}")),
            }
        }
        Err(format!(
            "no usable GPU adapter (tried {}): {}",
            errors.len(),
            errors.join("; ")
        ))
    }

    /// Two adapter handles refer to the same physical device+backend. Used to
    /// avoid re-trying the HighPerformance pick when it reappears in the
    /// enumerate_adapters() fallback list (AdapterInfo isn't Eq).
    fn same_adapter(a: &wgpu::AdapterInfo, b: &wgpu::AdapterInfo) -> bool {
        a.name == b.name && a.backend == b.backend && a.device == b.device && a.vendor == b.vendor
    }

    /// Initialize a device + pipeline + buffers on ONE adapter and gate it on
    /// the BLAKE3 self-check. Returns Err (never mines) on any failure so
    /// new_async can try the next candidate adapter — this is what turns a
    /// broken preferred backend into a fallback instead of a CPU demotion.
    async fn try_build(adapter: &wgpu::Adapter) -> Result<Self, String> {
        let info = adapter.get_info();
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("alphanumeric-gpu-miner"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .map_err(|e| format!("device init failed: {e}"))?;

        // build_checked (pipeline/buffers + self-check dispatch) is SYNCHRONOUS
        // and can PANIC on a broken driver — wgpu routes shader/pipeline
        // validation errors through a handler that panic!()s, and a wedged
        // device can panic in get_mapped_range during the self-check. Contain
        // that panic (the release profile is panic=unwind) so new_async falls
        // through to the NEXT candidate — e.g. DX12 when the Vulkan driver is
        // broken — instead of unwinding all the way out to a CPU demotion. This
        // is what lets the fallback cover the COMMON broken-driver case (a
        // crash), not just clean Err returns. AssertUnwindSafe is sound because a
        // caught panic discards this device/queue and the next candidate builds
        // a fresh one. (A driver that HANGS in poll(Wait) rather than panicking
        // still can't be recovered — an unavoidable limit, same as baseline.)
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            Self::build_checked(device, queue, &info)
        })) {
            Ok(res) => res,
            Err(_) => Err("driver panicked during pipeline/self-check init".into()),
        }
    }

    /// Synchronous: build the pipeline + buffers on an initialized device and
    /// gate on the BLAKE3 self-check. Split out so try_build can run it under
    /// catch_unwind. Production always goes through here (never build_unchecked
    /// directly), so no adapter mines without passing the self-check.
    fn build_checked(
        device: wgpu::Device,
        queue: wgpu::Queue,
        info: &wgpu::AdapterInfo,
    ) -> Result<Self, String> {
        let miner = Self::build_unchecked(device, queue, info);
        // Gate on BLAKE3 correctness BEFORE this adapter can ever mine: a kernel
        // that disagrees with the CPU blake3 crate rejects the adapter here and
        // the caller falls through to the next candidate.
        miner.self_check()?;
        Ok(miner)
    }

    /// Build the pipeline + buffers WITHOUT the self-check. Production always
    /// uses build_checked; only the tests construct this directly, so a broken
    /// kernel FAILS their explicit hash asserts loudly instead of the whole GPU
    /// suite silently skipping (which is what an internal self-check gate would
    /// cause via the `miner()` helper returning None).
    fn build_unchecked(device: wgpu::Device, queue: wgpu::Queue, info: &wgpu::AdapterInfo) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blake3-pow"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("blake3-pow"),
            layout: None,
            module: &shader,
            entry_point: "main",
            compilation_options: Default::default(),
            cache: None,
        });

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let result_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("result"),
            size: std::mem::size_of::<ResultBuf>() as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: std::mem::size_of::<ResultBuf>() as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_layout = pipeline.get_bind_group_layout(0);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blake3-pow"),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: result_buf.as_entire_binding(),
                },
            ],
        });

        Self {
            device,
            queue,
            pipeline,
            params_buf,
            result_buf,
            readback_buf,
            bind,
            adapter_name: format!("{} ({:?})", info.name, info.backend),
        }
    }

    fn header_words(header: &[u8; 92]) -> [u32; 24] {
        let mut w = [0u32; 24];
        for (i, chunk) in header.chunks(4).enumerate() {
            let mut b = [0u8; 4];
            b[..chunk.len()].copy_from_slice(chunk);
            w[i] = u32::from_le_bytes(b);
        }
        w
    }

    fn dispatch(&self, params: &Params, groups: u32) -> ResultBuf {
        self.queue
            .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(params));
        self.queue.write_buffer(
            &self.result_buf,
            0,
            bytemuck::bytes_of(&ResultBuf::zeroed()),
        );
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.dispatch_workgroups(groups, 1, 1);
        }
        enc.copy_buffer_to_buffer(
            &self.result_buf,
            0,
            &self.readback_buf,
            0,
            std::mem::size_of::<ResultBuf>() as u64,
        );
        self.queue.submit([enc.finish()]);

        let slice = self.readback_buf.slice(..);
        // poll(Wait) below IS the synchronization: wgpu 22 guarantees every
        // map_async callback has already fired by the time Maintain::Wait
        // returns. The callback is required by the API but need do nothing — the
        // old mpsc channel + rx.recv() added a per-dispatch allocation and a
        // no-op wait whose Result was discarded anyway. A failed map still
        // surfaces as a panic in get_mapped_range, exactly as before. DO NOT
        // remove poll(Wait) — deleting it (rather than the channel) deadlocks.
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let out: ResultBuf = *bytemuck::from_bytes(&slice.get_mapped_range());
        self.readback_buf.unmap();
        out
    }

    /// Search `threads * iters` nonces from `base_nonce` in ONE dispatch. Each
    /// thread tests `iters` consecutive nonces, so the single GPU->CPU readback is
    /// amortized across the whole batch (the ~10x throughput fix). Caller keeps
    /// `threads * iters <= 2^32` so the kernel's per-thread u32 offset is exact.
    pub fn search_batch_iters(
        &self,
        header: &[u8; 92],
        zero_bits: u32,
        base_nonce: u64,
        threads: u32,
        iters: u32,
    ) -> Option<u64> {
        debug_assert!(zero_bits != DEBUG_HASH_SENTINEL);
        let params = Params {
            header: Self::header_words(header),
            nonce_lo: base_nonce as u32,
            nonce_hi: (base_nonce >> 32) as u32,
            zero_bits,
            threads,
            iters,
            _pad: [0; 3],
        };
        let out = self.dispatch(&params, threads.div_ceil(WORKGROUP));
        if out.found != 0 {
            Some(((out.nonce_hi as u64) << 32) | out.nonce_lo as u64)
        } else {
            None
        }
    }

    /// Convenience: search `batch` nonces with one nonce per thread (used by tests).
    pub fn search_batch(
        &self,
        header: &[u8; 92],
        zero_bits: u32,
        base_nonce: u64,
        batch: u32,
    ) -> Option<u64> {
        self.search_batch_iters(header, zero_bits, base_nonce, batch, 1)
    }

    /// Search up to `max_nonces` nonces from `start_nonce` in batches, honoring
    /// `stop`. Returns the winning nonce, or None if exhausted/stopped.
    pub fn search(
        &self,
        header: &[u8; 92],
        zero_bits: u32,
        start_nonce: u64,
        max_nonces: u64,
        batch: u32,
        stop: &AtomicBool,
    ) -> Option<u64> {
        let mut done = 0u64;
        while done < max_nonces && !stop.load(Ordering::Relaxed) {
            let this = batch.min((max_nonces - done).min(u32::MAX as u64) as u32);
            if let Some(n) =
                self.search_batch(header, zero_bits, start_nonce.wrapping_add(done), this)
            {
                return Some(n);
            }
            done += this as u64;
        }
        None
    }

    /// Kernel self-check: BLAKE3 of the header with `nonce` computed ON THE GPU.
    /// Used by tests and the startup sanity check.
    pub fn hash_on_gpu(&self, header: &[u8; 92], nonce: u64) -> [u8; 32] {
        let params = Params {
            header: Self::header_words(header),
            nonce_lo: nonce as u32,
            nonce_hi: (nonce >> 32) as u32,
            zero_bits: DEBUG_HASH_SENTINEL,
            threads: 1,
            iters: 1,
            _pad: [0; 3],
        };
        let out = self.dispatch(&params, 1);
        let mut bytes = [0u8; 32];
        for (i, w) in out.hash.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        bytes
    }

    /// Cheap startup sanity check: one random header hashed on GPU must equal
    /// the CPU blake3 crate. Refuses to mine on a kernel that disagrees.
    pub fn self_check(&self) -> Result<(), String> {
        let mut header = [0u8; 92];
        for (i, b) in header.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        let nonce: u64 = 0x0123_4567_89AB_CDEF;
        header[44..52].copy_from_slice(&nonce.to_le_bytes());
        let gpu = self.hash_on_gpu(&header, nonce);
        let cpu = blake3::hash(&header);
        if gpu != *cpu.as_bytes() {
            return Err("GPU BLAKE3 kernel disagrees with CPU (self-check failed)".into());
        }
        Ok(())
    }
}

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::a9::blockchain::Block;

/// One physical GPU as the node reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuDevice {
    pub index: u32,
    pub name: String,
    pub backend: String,
    pub vendor: u32,
    pub device: u32,
}

/// One entry per physical card. `enumerate_adapters` lists a card once per
/// backend (Vulkan and GL on Linux; DX12 too on Windows); keep the first,
/// which is the preferred backend since the list is built in backend order.
pub fn dedup_physical(list: Vec<GpuDevice>) -> Vec<GpuDevice> {
    // GL names a card by its renderer string ("… /PCIe/SSE2") and reports
    // no PCI ids, so it cannot be matched to its Vulkan/DX12/Metal twin.
    // It is also never the backend to mine on when a real one is there:
    // drop every GL adapter as soon as any other backend listed anything.
    let has_real = list.iter().any(|d| d.backend != "Gl");
    let list: Vec<GpuDevice> = if has_real {
        list.into_iter().filter(|d| d.backend != "Gl").collect()
    } else {
        list
    };
    // Per vendor, the first backend that lists it sets the card count. A
    // later backend listing no more cards of that vendor is showing the same
    // cards again -- sometimes under an invented name and id, as Wine's DX12
    // layer does ("GTX 470" for an RTX 5090) -- so it adds none. One that
    // lists more keeps them, for a card only it can drive; its twins of the
    // first backend's cards then fall to the exact match below.
    let mut first_backend: Vec<(u32, String, usize)> = Vec::new(); // vendor, backend, count
    for d in &list {
        if !first_backend.iter().any(|(v, _, _)| *v == d.vendor) {
            let count = list
                .iter()
                .filter(|o| o.vendor == d.vendor && o.backend == d.backend)
                .count();
            first_backend.push((d.vendor, d.backend.clone(), count));
        }
    }
    let list: Vec<GpuDevice> = list
        .iter()
        .filter(|d| {
            // Every vendor was recorded above; a miss keeps the adapter.
            let Some((_, primary, primary_count)) =
                first_backend.iter().find(|(v, _, _)| *v == d.vendor)
            else {
                return true;
            };
            if &d.backend == primary {
                return true;
            }
            let here = list
                .iter()
                .filter(|o| o.vendor == d.vendor && o.backend == d.backend)
                .count();
            here > *primary_count
        })
        .cloned()
        .collect();
    let mut out: Vec<GpuDevice> = Vec::new();
    for d in list {
        if !out
            .iter()
            .any(|o| o.vendor == d.vendor && o.device == d.device && o.name == d.name)
        {
            out.push(d);
        }
    }
    for (i, d) in out.iter_mut().enumerate() {
        d.index = i as u32;
    }
    out
}

fn gpu_instance() -> (wgpu::Instance, wgpu::Backends) {
    let backends = wgpu::util::backend_bits_from_env().unwrap_or(wgpu::Backends::all());
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..Default::default()
    });
    (instance, backends)
}

fn device_of(info: &wgpu::AdapterInfo, index: u32) -> GpuDevice {
    GpuDevice {
        index,
        name: info.name.clone(),
        backend: format!("{:?}", info.backend),
        vendor: info.vendor,
        device: info.device,
    }
}

static ADAPTERS: std::sync::OnceLock<Vec<GpuDevice>> = std::sync::OnceLock::new();

/// The usable adapters, enumerated once per process. Software adapters
/// (llvmpipe) are left out, the way `new_async` leaves them out. A driver
/// that panics during enumeration reads as no adapters.
pub fn usable_adapters() -> Vec<GpuDevice> {
    ADAPTERS
        .get_or_init(|| {
            std::panic::catch_unwind(|| {
                let (instance, backends) = gpu_instance();
                let raw: Vec<GpuDevice> = instance
                    .enumerate_adapters(backends)
                    .into_iter()
                    .map(|a| a.get_info())
                    .filter(|gi| gi.device_type != wgpu::DeviceType::Cpu)
                    .enumerate()
                    .map(|(i, gi)| device_of(&gi, i as u32))
                    .collect();
                dedup_physical(raw)
            })
            .unwrap_or_default()
        })
        .clone()
}

/// Which usable adapters to mine on. `ALPHANUMERIC_GPU_INDEX=N` means exactly
/// device N; `ALPHANUMERIC_GPU_DEVICES=0,2` a set; neither, every one.
pub fn select_indices(
    available: &[GpuDevice],
    devices_env: Option<&str>,
    index_env: Option<&str>,
) -> Result<Vec<u32>, String> {
    let n = available.len() as u32;
    let check = |i: u32| -> Result<u32, String> {
        if i < n {
            Ok(i)
        } else {
            Err(format!(
                "GPU index {i} is out of range: {n} usable GPU(s) found (valid 0..={})",
                n.saturating_sub(1)
            ))
        }
    };
    if let Some(one) = index_env {
        let i: u32 = one
            .trim()
            .parse()
            .map_err(|_| format!("ALPHANUMERIC_GPU_INDEX={one:?} is not a number"))?;
        return Ok(vec![check(i)?]);
    }
    if let Some(list) = devices_env {
        let mut picked = Vec::new();
        for part in list.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let i: u32 = part
                .parse()
                .map_err(|_| format!("ALPHANUMERIC_GPU_DEVICES entry {part:?} is not a number"))?;
            let i = check(i)?;
            if !picked.contains(&i) {
                picked.push(i);
            }
        }
        picked.sort_unstable();
        if picked.is_empty() {
            return Err("ALPHANUMERIC_GPU_DEVICES is set but names no GPU".into());
        }
        return Ok(picked);
    }
    Ok((0..n).collect())
}

impl GpuMiner {
    /// Build on the physical card at `index` in [`usable_adapters`] order,
    /// trying each backend that card is listed under (Vulkan first, then GL or
    /// DX12) so a broken preferred backend falls back on the SAME card.
    pub fn new_for_index(index: u32) -> Result<Self, String> {
        pollster::block_on(async {
            let target = usable_adapters()
                .into_iter()
                .find(|d| d.index == index)
                .ok_or_else(|| format!("no usable GPU at index {index}"))?;
            let (instance, backends) = gpu_instance();
            let mut errors = Vec::new();
            for adapter in instance.enumerate_adapters(backends) {
                let gi = adapter.get_info();
                if gi.device_type == wgpu::DeviceType::Cpu
                    || gi.vendor != target.vendor
                    || gi.device != target.device
                    || gi.name != target.name
                {
                    continue;
                }
                match Self::try_build(&adapter).await {
                    Ok(miner) => return Ok(miner),
                    Err(e) => errors.push(format!("{} ({:?}): {e}", gi.name, gi.backend)),
                }
            }
            Err(if errors.is_empty() {
                format!("GPU {index} ({}) is no longer enumerable", target.name)
            } else {
                errors.join("; ")
            })
        })
    }
}

/// Start of device slot `slot` of `slots` in the 64-bit nonce space: slots
/// are `u64::MAX / slots` apart, the first at `base`.
/// `(nonce, timestamp, difficulty, hash)` of a found block.
pub type Hit = (u64, u64, u64, [u8; 32]);

pub fn nonce_base_for_slot(base: u64, slot: usize, slots: usize) -> u64 {
    let stride = u64::MAX / slots.max(1) as u64;
    base.wrapping_add(stride.wrapping_mul(slot as u64))
}

/// Per-device counters. Every field is an atomic so the stats server reads
/// them without a lock while the device threads write.
struct DeviceStats {
    index: u32,
    name: String,
    /// EWMA GH/s as f64 bits, carried across attempts.
    rate_ewma_bits: AtomicU64,
    hashes: AtomicU64,
    alive: AtomicBool,
    /// Converged dispatch size, carried across attempts (was the global
    /// LAST_ITERS; each card converges to its own speed).
    last_iters: std::sync::atomic::AtomicU32,
}

impl DeviceStats {
    fn new(index: u32, name: String) -> Self {
        Self {
            index,
            name,
            rate_ewma_bits: AtomicU64::new(0f64.to_bits()),
            hashes: AtomicU64::new(0),
            alive: AtomicBool::new(true),
            last_iters: std::sync::atomic::AtomicU32::new(4),
        }
    }
}

/// What the stats server publishes for one GPU.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSnapshot {
    pub index: u32,
    pub name: String,
    /// Hashes per second.
    pub hps: f64,
    pub hashes: u64,
    pub alive: bool,
}

/// Every selected card, each with its own miner and counters.
pub struct GpuPool {
    miners: Vec<Arc<GpuMiner>>,
    stats: Vec<DeviceStats>,
}

impl GpuPool {
    fn build() -> Result<Self, String> {
        let all = usable_adapters();
        let chosen = select_indices(
            &all,
            std::env::var("ALPHANUMERIC_GPU_DEVICES").ok().as_deref(),
            std::env::var("ALPHANUMERIC_GPU_INDEX").ok().as_deref(),
        )?;
        let mut miners = Vec::new();
        let mut stats = Vec::new();
        let mut errors = Vec::new();
        for i in chosen {
            let d = &all[i as usize];
            let built = std::panic::catch_unwind(|| GpuMiner::new_for_index(i))
                .unwrap_or_else(|_| Err("GPU init panicked (driver crash)".to_string()));
            match built {
                Ok(m) => {
                    miners.push(Arc::new(m));
                    stats.push(DeviceStats::new(d.index, d.name.clone()));
                }
                Err(e) => errors.push(format!("[{}] {}: {e}", d.index, d.name)),
            }
        }
        if miners.is_empty() {
            return Err(format!(
                "no usable GPU adapter ({})",
                if errors.is_empty() {
                    "none found".to_string()
                } else {
                    errors.join("; ")
                }
            ));
        }
        for e in &errors {
            eprintln!("  GPU skipped: {e}");
        }
        Ok(Self { miners, stats })
    }

    /// The same physical adapter built `copies` times -- two "devices" on a
    /// one-GPU box, so the coordinator's multi-device paths run in a test.
    #[cfg(test)]
    pub fn build_for_test(copies: usize) -> Result<Self, String> {
        let d = usable_adapters().first().ok_or("no GPU")?.clone();
        let mut miners = Vec::new();
        let mut stats = Vec::new();
        for k in 0..copies {
            miners.push(Arc::new(GpuMiner::new_for_index(d.index)?));
            stats.push(DeviceStats::new(k as u32, d.name.clone()));
        }
        Ok(Self { miners, stats })
    }

    /// Slots of the devices that have not died.
    pub fn live(&self) -> Vec<usize> {
        (0..self.miners.len())
            .filter(|&i| self.stats[i].alive.load(Ordering::Relaxed))
            .collect()
    }

    pub fn snapshots(&self) -> Vec<DeviceSnapshot> {
        self.stats
            .iter()
            .map(|s| {
                let ghs = f64::from_bits(s.rate_ewma_bits.load(Ordering::Relaxed));
                DeviceSnapshot {
                    index: s.index,
                    name: s.name.clone(),
                    hps: if ghs.is_finite() { ghs * 1e9 } else { 0.0 },
                    hashes: s.hashes.load(Ordering::Relaxed),
                    alive: s.alive.load(Ordering::Relaxed),
                }
            })
            .collect()
    }

    fn publish_total_rate(&self) {
        let total: f64 = self
            .stats
            .iter()
            .filter(|s| s.alive.load(Ordering::Relaxed))
            .map(|s| f64::from_bits(s.rate_ewma_bits.load(Ordering::Relaxed)))
            .filter(|g| g.is_finite())
            .sum();
        RATE_EWMA_BITS.store(total.to_bits(), Ordering::Relaxed);
    }

    fn reset_rates(&self) {
        for s in &self.stats {
            s.rate_ewma_bits.store(0f64.to_bits(), Ordering::Relaxed);
        }
    }

    /// One block attempt on every live device at once. Device slot `k` of `n`
    /// searches from `nonce_base_for_slot(base, k, n)`; the first hit stops
    /// the others at their next dispatch boundary. A device that panics is
    /// marked dead and skipped from then on; when the last one dies the panic
    /// is re-raised so the caller's existing demote-to-CPU path runs.
    #[allow(clippy::too_many_arguments)]
    pub fn attempt(
        &self,
        number: u32,
        previous_hash: &[u8; 32],
        merkle_root: &[u8; 32],
        previous_difficulty: u64,
        previous_block_timestamp: u64,
        budget: Duration,
        tip_counter: &AtomicU64,
        tip_version: u64,
        session_progress_micro: &AtomicU64,
        stop: &AtomicBool,
    ) -> Option<Hit> {
        let live = self.live();
        if live.is_empty() {
            return None;
        }
        let hit: Mutex<Option<Hit>> = Mutex::new(None);
        let found = AtomicBool::new(false);
        let died: Mutex<Option<String>> = Mutex::new(None);
        let base = crate::a9::miner::attempt_nonce_base();
        let slots = live.len();
        let deadline = Instant::now() + budget;
        std::thread::scope(|scope| {
            for (k, &slot) in live.iter().enumerate() {
                let miner = Arc::clone(&self.miners[slot]);
                let (hit, found, died) = (&hit, &found, &died);
                scope.spawn(move || {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.device_search(
                            &miner,
                            slot,
                            nonce_base_for_slot(base, k, slots),
                            number,
                            previous_hash,
                            merkle_root,
                            previous_difficulty,
                            previous_block_timestamp,
                            deadline,
                            tip_counter,
                            tip_version,
                            session_progress_micro,
                            stop,
                            found,
                        )
                    }));
                    match r {
                        Ok(Some(h)) => {
                            if let Ok(mut g) = hit.lock() {
                                if g.is_none() {
                                    *g = Some(h);
                                }
                            }
                            found.store(true, Ordering::SeqCst);
                        }
                        Ok(None) => {}
                        Err(payload) => {
                            let s = &self.stats[slot];
                            s.alive.store(false, Ordering::Relaxed);
                            s.rate_ewma_bits.store(0f64.to_bits(), Ordering::Relaxed);
                            let why = payload
                                .downcast_ref::<String>()
                                .cloned()
                                .or_else(|| payload.downcast_ref::<&str>().map(|m| m.to_string()))
                                .unwrap_or_else(|| "device panicked".to_string());
                            eprintln!("  GPU [{}] {} lost mid-session: {why}", s.index, s.name);
                            if let Ok(mut d) = died.lock() {
                                *d = Some(format!("[{}] {}: {why}", s.index, s.name));
                            }
                        }
                    }
                });
            }
        });
        self.publish_total_rate();
        let result = hit.into_inner().ok().flatten();
        if result.is_none() && self.live().is_empty() {
            let why = died
                .into_inner()
                .ok()
                .flatten()
                .unwrap_or_else(|| "every GPU died".to_string());
            // Unwinds like a device panic would, so the caller's existing
            // `note_gpu_died` + CPU-fallback path (miner.rs) handles it.
            std::panic::resume_unwind(Box::new(format!("every GPU in the pool died: {why}")));
        }
        result
    }

    /// The single-card search loop, run on one device thread. See
    /// [`gpu_mine_attempt`] for the dispatch sizing, preemption and display
    /// rationale; this is that loop with per-device counters and the shared
    /// `found` flag as an extra stop condition.
    #[allow(clippy::too_many_arguments)]
    fn device_search(
        &self,
        gpu: &GpuMiner,
        slot: usize,
        mut base: u64,
        number: u32,
        previous_hash: &[u8; 32],
        merkle_root: &[u8; 32],
        previous_difficulty: u64,
        previous_block_timestamp: u64,
        deadline: Instant,
        tip_counter: &AtomicU64,
        tip_version: u64,
        session_progress_micro: &AtomicU64,
        stop: &AtomicBool,
        found: &AtomicBool,
    ) -> Option<(u64, u64, u64, [u8; 32])> {
        const THREADS: u32 = 65535 * 256;
        const MAX_ITERS: u32 = 256;
        let stats = &self.stats[slot];
        let target_dispatch_ms =
            adaptive_dispatch_target_ms(TIP_INTERVAL_EWMA_MS.load(Ordering::Acquire));
        let tip_moved = || tip_counter.load(Ordering::Acquire) != tip_version;
        let mut iters: u32 = stats.last_iters.load(Ordering::Relaxed).clamp(1, MAX_ITERS);
        while Instant::now() < deadline
            && !tip_moved()
            && !stop.load(Ordering::Relaxed)
            && !found.load(Ordering::Relaxed)
        {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .max(previous_block_timestamp);
            let difficulty = Block::consensus_next_difficulty(
                previous_difficulty,
                timestamp.saturating_sub(previous_block_timestamp),
                number,
            );
            let zero_bits = (difficulty / 16) as u32;
            let header = build_header(number, previous_hash, timestamp, 0, difficulty, merkle_root);

            let per_dispatch = THREADS as u64 * iters as u64;
            let dispatch_start = Instant::now();
            let nonce = gpu.search_batch_iters(&header, zero_bits, base, THREADS, iters);
            stats.hashes.fetch_add(per_dispatch, Ordering::Relaxed);
            if let Some(nonce) = nonce {
                if tip_moved() {
                    record_tip_change_observation();
                    return None;
                }
                let full = build_header(
                    number,
                    previous_hash,
                    timestamp,
                    nonce,
                    difficulty,
                    merkle_root,
                );
                let hash = *blake3::hash(&full).as_bytes();
                return Some((nonce, timestamp, difficulty, hash));
            }
            let dispatch_ms = dispatch_start.elapsed().as_secs_f64() * 1000.0;
            base = base.wrapping_add(per_dispatch);
            iters = next_dispatch_iters(iters, dispatch_ms, target_dispatch_ms, MAX_ITERS);
            stats.last_iters.store(iters, Ordering::Relaxed);

            let inst_ghs = per_dispatch as f64 / (dispatch_ms.max(1.0) / 1000.0) / 1e9;
            let prev = f64::from_bits(stats.rate_ewma_bits.load(Ordering::Relaxed));
            let ghs = if prev.is_finite() && prev > 0.0 {
                prev * 0.8 + inst_ghs * 0.2
            } else {
                inst_ghs
            };
            stats.rate_ewma_bits.store(ghs.to_bits(), Ordering::Relaxed);
            self.publish_total_rate();
            LAST_DIFFICULTY.store(difficulty, Ordering::Relaxed);
            let progress_inc =
                ((per_dispatch as f64 / expected_hashes(difficulty)) * 1e6).max(0.0) as u64;
            session_progress_micro.fetch_add(progress_inc, Ordering::Relaxed);
        }
        if tip_moved() {
            record_tip_change_observation();
        }
        None
    }
}

/// Per-GPU figures of the cached pool; empty before the pool is built or
/// while it is in the failed state.
pub fn device_snapshots() -> Vec<DeviceSnapshot> {
    let cache = GPU.lock().unwrap_or_else(|p| p.into_inner());
    match &*cache {
        GpuCache::Ready(pool) => pool.snapshots(),
        _ => Vec::new(),
    }
}

/// Process-wide cached GPU miner (init is ~100-200ms; reuse it across blocks).
///
/// REBUILDABLE (was a write-once OnceLock): a mid-session device loss — Windows
/// TDR, a driver reset, an eGPU unplug — used to poison the cache permanently,
/// demoting to CPU for the rest of the PROCESS even though the GPU usually
/// recovers in ~2s. Now the cache can be rebuilt: gpu_status() detects the dead
/// device on the next command and calls rebuild_gpu(), resuming GPU mining. The
/// miner is held behind an Arc so the hot mining path clones a handle under a
/// short lock and a rebuild can swap the cached miner without disturbing an
/// in-flight attempt (which keeps its old Arc, fails once, and picks up the new
/// one next attempt). Failed keeps the reason (shown to the user — the default
/// log filter is Error-only) plus a backoff clock so a truly-dead GPU doesn't
/// thrash re-creating a device every command. Recovery is split by WHERE the
/// death is detected: an IDLE loss (device reset between commands) leaves the
/// cache Ready(dead) and gpu_status rebuilds it IMMEDIATELY on the next command
/// (likely a transient TDR, recovers in ~2s); an UNDER-LOAD loss (the mining
/// dispatch panics mid-attempt) routes through note_gpu_died() into Failed, so
/// the backoff throttles it (a card that dies under load is likely FLAPPING —
/// marginal PSU/thermal/OC — and re-initializing wgpu every block is pure waste).
enum GpuCache {
    Uninit,
    Ready(Arc<GpuPool>),
    Failed { reason: String, since: Instant },
}

static GPU: Mutex<GpuCache> = Mutex::new(GpuCache::Uninit);

/// Min gap between rebuild attempts once the GPU is in the Failed state, so a
/// persistently-dead / flapping card retries at most ~once per this window
/// instead of re-initializing wgpu (~100-200ms) + burning a crashed attempt on
/// every mine command. Monotonic Instant (not wall-clock) so an NTP step can't
/// perturb recovery timing.
const REBUILD_BACKOFF: Duration = Duration::from_secs(30);

/// Displayed-rate EWMA (f64 bits), carried across attempts. Instantaneous
/// per-dispatch rate keeps template-rebuild gaps out of the denominator, and
/// the cross-attempt EWMA keeps the number steady through ~5s tip churn —
/// an attempt-local average sagged ~40% at every tip change and read as
/// thermal throttling on a perfectly healthy card.
static RATE_EWMA_BITS: AtomicU64 = AtomicU64::new(0);

/// Observed network tip-change cadence, carried across attempts: epoch-ms of
/// the last observed tip change + an EWMA of the intervals between changes.
/// Feeds the adaptive dispatch target — the optimum dispatch length depends on
/// how often the tip actually moves (D* = sqrt(2·T_o·T_tip)), and the live
/// cadence swings between ~2s (difficulty climbing) and the 5s target
/// (equilibrium). A hardcoded target tuned to either end is mistuned at the
/// other; measuring T_tip keeps the sizing correct at both without retunes.
static LAST_TIP_CHANGE_EPOCH_MS: AtomicU64 = AtomicU64::new(0);
static TIP_INTERVAL_EWMA_MS: AtomicU64 = AtomicU64::new(0);

/// Record that a tip change was just observed by the dispatch loop. EWMA over
/// the inter-change intervals, with a sanity window so counter bursts (<200ms)
/// and idle gaps between mine commands (>60s) never poison the cadence.
fn record_tip_change_observation() {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let prev = LAST_TIP_CHANGE_EPOCH_MS.swap(now_ms, std::sync::atomic::Ordering::AcqRel);
    if prev == 0 {
        return;
    }
    let interval = now_ms.saturating_sub(prev);
    if !(200..=60_000).contains(&interval) {
        return;
    }
    let prev_ewma = TIP_INTERVAL_EWMA_MS.load(std::sync::atomic::Ordering::Acquire);
    let next = if prev_ewma == 0 {
        interval
    } else {
        (prev_ewma * 7 + interval * 3) / 10
    };
    TIP_INTERVAL_EWMA_MS.store(next, std::sync::atomic::Ordering::Release);
}

/// Difficulty of the most recent dispatch — read by the display task.
static LAST_DIFFICULTY: AtomicU64 = AtomicU64::new(0);

/// (Re)build the miner into `cache` and return a handle. GpuMiner::new() self-
/// checks each adapter inside its fallback loop, so an Ok here is a verified-
/// correct BLAKE3 kernel on a working adapter (a broken preferred backend having
/// fallen back, not demoted). Records Failed with a fresh backoff clock on error.
fn build_into(cache: &mut GpuCache) -> Result<Arc<GpuPool>, String> {
    // catch_unwind: a wedged driver's wgpu error handler can panic in
    // Instance::new / request_adapter / request_device, before any guarded
    // region. Without catching it here the panic would unwind through the held
    // Mutex guard, poison the lock AND skip the `*cache = Failed` write, so the
    // backoff would never engage. (Each device build inside GpuPool::build is
    // guarded too; this catches enumeration and anything else.)
    let built = std::panic::catch_unwind(GpuPool::build)
        .unwrap_or_else(|_| Err("GPU init panicked (driver crash)".to_string()));
    match built {
        Ok(pool) => {
            // A fresh pool starts every device's adaptive dispatch size from the
            // floor (DeviceStats::new), so a rebuilt device can't inherit an old
            // converged size and run for seconds before shrinking.
            let arc = Arc::new(pool);
            *cache = GpuCache::Ready(Arc::clone(&arc));
            Ok(arc)
        }
        Err(e) => {
            *cache = GpuCache::Failed {
                reason: e.clone(),
                since: Instant::now(),
            };
            Err(e)
        }
    }
}

/// A live miner handle, building it on first use and rebuilding a Failed cache
/// once its backoff has elapsed. Cheap on the hot path (Arc clone under a short
/// lock). The build runs while holding the lock — this serializes concurrent
/// first-inits exactly like the old OnceLock::get_or_init, and there is no await
/// held across the std Mutex (GpuMiner::new() is synchronous).
fn shared_gpu_arc() -> Result<Arc<GpuPool>, String> {
    let mut cache = GPU.lock().unwrap_or_else(|p| p.into_inner());
    match &*cache {
        GpuCache::Ready(m) => return Ok(Arc::clone(m)),
        GpuCache::Failed { reason, since } => {
            if since.elapsed() < REBUILD_BACKOFF {
                return Err(reason.clone()); // within backoff: don't thrash re-init
            }
            // past backoff: fall through and retry the build
        }
        GpuCache::Uninit => {} // first use: fall through and build
    }
    build_into(&mut cache)
}

/// Force a rebuild NOW, ignoring the Failed backoff — called only when a re-check
/// has just proved the cached device dead (an IDLE loss, cache still Ready), so a
/// transient TDR (recovers in ~2s) resumes GPU mining immediately instead of
/// waiting out the backoff window. (An UNDER-LOAD loss goes through
/// note_gpu_died() into Failed, so it is backoff-throttled, not rebuilt here.)
fn rebuild_gpu() -> Result<Arc<GpuPool>, String> {
    let mut cache = GPU.lock().unwrap_or_else(|p| p.into_inner());
    build_into(&mut cache)
}

/// Record that the GPU died UNDER LOAD (mid-attempt) — called from the miner's
/// spawn_blocking JoinError arm. Poisons the cache to Failed so the NEXT command
/// backs off to CPU for REBUILD_BACKOFF instead of re-initializing wgpu + burning
/// a crashed attempt every block. A card that dies specifically under mining load
/// is likely FLAPPING (marginal PSU/thermal/OC), so throttling is right; a benign
/// transient that happened to hit under load simply waits out one backoff window
/// (still self-heals, unlike the old permanent-CPU demotion).
pub fn note_gpu_died(reason: &str) {
    let mut cache = GPU.lock().unwrap_or_else(|p| p.into_inner());
    *cache = GpuCache::Failed {
        reason: format!("GPU lost mid-session: {reason}"),
        since: Instant::now(),
    };
}

/// Adapter name + backend if the GPU is usable, or the reason it is not.
/// The mine command prints this ONCE on stdout so `--gpu` is never silent
/// about which adapter it picked (or that it picked none).
pub fn gpu_status() -> Result<String, String> {
    let pool = shared_gpu_arc()?;
    // Re-verify per mine command: a cached-Ready device can have died since the
    // last command (driver reset/TDR) -- without this the status line would claim
    // a healthy GPU on a dead device. One 92-byte hash per device (~ms). A dead
    // device can make the readback PANIC rather than return Err, so catch that
    // too and treat it as a loss.
    let all_alive = pool.live().len() == pool.miners.len()
        && pool.miners.iter().all(|m| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| m.self_check()))
                .unwrap_or_else(|_| Err("device panicked during re-check".into()))
                .is_ok()
        });
    let pool = if all_alive {
        pool
    } else {
        // Rebuild once now (each device self-checks and falls back across its
        // backends). A transient TDR recovers in ~2s so GPU mining resumes this
        // command; a persistently-dead pool caches Failed and the backoff
        // throttles further attempts.
        rebuild_gpu()?
    };
    pool.reset_rates();
    Ok(pool
        .stats
        .iter()
        .map(|s| s.name.clone())
        .collect::<Vec<_>>()
        .join(", "))
}

fn shared_gpu() -> Option<Arc<GpuPool>> {
    shared_gpu_arc().ok()
}

/// Build the 92-byte mining header (matches the CPU miner's layout exactly).
fn build_header(
    number: u32,
    previous_hash: &[u8; 32],
    timestamp: u64,
    nonce: u64,
    difficulty: u64,
    merkle_root: &[u8; 32],
) -> [u8; 92] {
    let mut h = [0u8; 92];
    h[0..4].copy_from_slice(&number.to_le_bytes());
    h[4..36].copy_from_slice(previous_hash);
    h[36..44].copy_from_slice(&timestamp.to_le_bytes());
    h[44..52].copy_from_slice(&nonce.to_le_bytes());
    h[52..60].copy_from_slice(&difficulty.to_le_bytes());
    h[60..92].copy_from_slice(merkle_root);
    h
}

/// Expected hashes to solve one block at `difficulty` (target = MAX >> (d/16)).
fn expected_hashes(difficulty: u64) -> f64 {
    2f64.powi((difficulty / 16).min(255) as i32)
}

/// Clear the display statics at the start of a mine command so a second
/// `mine --gpu` in the same process doesn't flash the previous command's
/// GH/s and difficulty for ~1s before the first new dispatch lands.
pub fn reset_display_state() {
    RATE_EWMA_BITS.store(0, std::sync::atomic::Ordering::Relaxed);
    LAST_DIFFICULTY.store(0, std::sync::atomic::Ordering::Relaxed);
    // Re-arm the tip-cadence sampler (prev==0 discards the next interval):
    // this runs at the start of every mining round, so the sampler never
    // bridges a miner-idle gap — win finalize + absorption (≤20s) + jitter,
    // or a ≤60s pause between commands — into a fake "tip interval" that
    // inflates the EWMA and drags the adaptive dispatch target off cadence.
    // The interval EWMA itself is kept: genuine samples stay valid across
    // rounds; only the bridge sample is discarded.
    LAST_TIP_CHANGE_EPOCH_MS.store(0, std::sync::atomic::Ordering::Release);
}

/// Live display readings for the mine command's bar task: (EWMA GH/s, last
/// difficulty mined against). The GPU thread only ever writes atomics — it
/// must NEVER touch the console (see gpu_mine_attempt's display note).
pub fn gpu_display_snapshot() -> (f64, u64) {
    let ghs = f64::from_bits(RATE_EWMA_BITS.load(std::sync::atomic::Ordering::Relaxed));
    let difficulty = LAST_DIFFICULTY.load(std::sync::atomic::Ordering::Relaxed);
    (if ghs.is_finite() { ghs } else { 0.0 }, difficulty)
}

/// Poisson mean seconds to one block at `difficulty` for a rate in GH/s.
pub fn expected_block_seconds(difficulty: u64, ghs: f64) -> f64 {
    expected_hashes(difficulty) / (ghs * 1e9).max(1.0)
}

/// Human "about how long" at a measured rate — the honest solo-mining ETA the
/// display owes the user (a Poisson mean, not a countdown).
pub fn format_eta(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "…".into();
    }
    if seconds < 90.0 {
        format!("~{:.0}s", seconds)
    } else if seconds < 5400.0 {
        format!("~{:.0}m", seconds / 60.0)
    } else if seconds < 172_800.0 {
        format!("~{:.1}h", seconds / 3600.0)
    } else {
        format!("~{:.1}d", seconds / 86_400.0)
    }
}

/// GPU nonce search for one block attempt. Refreshes the timestamp/difficulty
/// per sub-batch (like the CPU miner), searching until it finds a winning nonce,
/// hits the wall-clock `budget`, or the network tip moves (tip_counter no longer
/// equals tip_version — so a block someone else mined ends this attempt in ~1
/// dispatch instead of wasting the rest of the budget on a stale template). On
/// success returns `(nonce, timestamp, difficulty, hash)` for the existing CPU
/// finalizer to build+verify — the GPU only proposes a nonce; consensus unchanged.
///
/// Display: this thread NEVER touches the console. It only writes atomics —
/// `session_progress_micro` (cumulative expected-blocks of work, micro-units)
/// plus the rate/difficulty statics — and the mine command's display task
/// paints the bar from them at its own cadence. The first version called
/// indicatif setters from this loop between dispatches; indicatif draws on the
/// CALLING thread, and Windows console writes stall for 100ms+ — the GPU sat
/// idle behind console I/O, oscillating 40-70% utilization in Task Manager on
/// a 5s-block network where every ms between dispatches is paid at the tip
/// cadence. Progress accumulates PER-DISPATCH against the difficulty that work
/// was actually done at (the Poisson intensity integral): monotonic even while
/// live difficulty flaps across a /16 band boundary, where an
/// instant-difficulty denominator would halve/double the shown percent.
#[allow(clippy::too_many_arguments)]
pub fn gpu_mine_attempt(
    number: u32,
    previous_hash: &[u8; 32],
    merkle_root: &[u8; 32],
    previous_difficulty: u64,
    previous_block_timestamp: u64,
    budget: std::time::Duration,
    tip_counter: &std::sync::atomic::AtomicU64,
    tip_version: u64,
    session_progress_micro: &AtomicU64,
    stop: &std::sync::atomic::AtomicBool,
) -> Option<(u64, u64, u64, [u8; 32])> {
    let pool = shared_gpu()?;
    pool.attempt(
        number,
        previous_hash,
        merkle_root,
        previous_difficulty,
        previous_block_timestamp,
        budget,
        tip_counter,
        tip_version,
        session_progress_micro,
        stop,
    )
}

/// Optimal dispatch wall-clock target for the measured tip-change cadence:
/// D* = sqrt(2·T_o·T_tip) with T_o ≈ 1.5ms, clamped to [50, 250]ms; 77ms
/// (the ~2s-cadence optimum) until the first interval is measured. Pure so
/// the operating points are testable.
fn adaptive_dispatch_target_ms(tip_interval_ewma_ms: u64) -> f64 {
    const DISPATCH_OVERHEAD_MS: f64 = 1.5;
    const FALLBACK_TARGET_MS: f64 = 77.0;
    const MIN_TARGET_MS: f64 = 50.0;
    const MAX_TARGET_MS: f64 = 250.0;
    if tip_interval_ewma_ms == 0 {
        return FALLBACK_TARGET_MS;
    }
    (2.0 * DISPATCH_OVERHEAD_MS * tip_interval_ewma_ms as f64)
        .sqrt()
        .clamp(MIN_TARGET_MS, MAX_TARGET_MS)
}

/// Next dispatch size (iterations per thread) so one dispatch takes ~target_ms:
/// the between-dispatch tip check is the ONLY preemption point (a submitted
/// dispatch cannot be aborted), so dispatch wall-clock bounds how stale a
/// template can get. Pure so the scaling/clamping is testable. A measured time
/// of ~0 (timer glitch) leaves the size unchanged.
fn next_dispatch_iters(current: u32, measured_ms: f64, target_ms: f64, max_iters: u32) -> u32 {
    if !measured_ms.is_finite() || measured_ms < 1.0 {
        return current;
    }
    let scaled = (current as f64 * (target_ms / measured_ms)).round();
    if !scaled.is_finite() {
        return current;
    }
    (scaled as i64).clamp(1, max_iters as i64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The adaptive sizing must converge toward the wall-clock target and stay
    /// inside [1, max] — dispatch size is the tip-check preemption granularity,
    /// so a slow adapter MUST shrink to ~250ms batches (the "GPU always a few
    /// blocks behind" fix) and a fast one must grow toward the readback-
    /// amortizing cap.
    #[test]
    #[allow(clippy::assertions_on_constants)] // Named proof of the live dispatch cap invariant.
    fn dispatch_iters_adapt_toward_target() {
        // Slow adapter: 4 iters took 2s -> shrink to the floor.
        assert_eq!(next_dispatch_iters(4, 2000.0, 250.0, 64), 1);
        // Fast adapter: 4 iters in 20ms -> grow proportionally (4 * 250/20).
        assert_eq!(next_dispatch_iters(4, 20.0, 250.0, 64), 50);
        // Above the cap: clamp.
        assert_eq!(next_dispatch_iters(64, 100.0, 250.0, 64), 64);
        // The live cap (256) keeps THREADS*iters = 65535*256*256 < 2^32.
        assert!(65_535u64 * 256 * 256 < 1u64 << 32);
        assert_eq!(next_dispatch_iters(128, 50.0, 250.0, 256), 256);
        // On target: stable.
        assert_eq!(next_dispatch_iters(16, 250.0, 250.0, 64), 16);
        // Timer glitch (sub-ms measurement): unchanged.
        assert_eq!(next_dispatch_iters(8, 0.0, 250.0, 64), 8);
        assert_eq!(next_dispatch_iters(8, f64::NAN, 250.0, 64), 8);
    }

    /// The adaptive target must hit the documented operating points of
    /// D* = sqrt(2·T_o·T_tip) and stay inside its clamps at the extremes.
    #[test]
    fn adaptive_dispatch_target_tracks_cadence() {
        // No measurement yet: deploy-time fallback.
        assert_eq!(adaptive_dispatch_target_ms(0), 77.0);
        // ~2s cadence (difficulty climbing): ≈77ms.
        assert!((adaptive_dispatch_target_ms(2_000) - 77.46).abs() < 0.1);
        // 5s equilibrium cadence: ≈122ms.
        assert!((adaptive_dispatch_target_ms(5_000) - 122.47).abs() < 0.1);
        // Sub-second churn clamps at the floor (preemption never coarser-bounded
        // than 50ms), multi-minute cadence at the ceiling.
        assert_eq!(adaptive_dispatch_target_ms(500), 50.0);
        assert_eq!(adaptive_dispatch_target_ms(60_000), 250.0);
    }

    /// Build a miner on the best adapter WITHOUT the internal self-check, so the
    /// correctness tests below run their OWN explicit hash asserts and FAIL
    /// LOUDLY on a broken kernel. Production's GpuMiner::new() self-checks
    /// internally (the adapter-fallback gate), which would instead make this
    /// return None and SILENTLY SKIP the whole GPU suite on a broken kernel —
    /// exactly the regression a correctness test must not have. Returns None only
    /// when no GPU adapter/device exists (a legitimate skip on a headless box).
    fn miner() -> Option<GpuMiner> {
        let m = pollster::block_on(async {
            let backends = wgpu::util::backend_bits_from_env().unwrap_or(wgpu::Backends::all());
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..Default::default()
            });
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await?;
            let info = adapter.get_info();
            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("alphanumeric-gpu-miner-test"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::downlevel_defaults(),
                        memory_hints: wgpu::MemoryHints::Performance,
                    },
                    None,
                )
                .await
                .ok()?;
            Some(GpuMiner::build_unchecked(device, queue, &info))
        });
        if m.is_none() {
            eprintln!("skipping GPU tests: no usable GPU adapter/device");
        }
        m
    }

    fn header_with_nonce(seed: u8, nonce: u64) -> [u8; 92] {
        let mut h = [0u8; 92];
        for (i, b) in h.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(seed).wrapping_add(seed);
        }
        h[44..52].copy_from_slice(&nonce.to_le_bytes());
        h
    }

    #[test]
    fn gpu_hash_matches_cpu_blake3() {
        let Some(m) = miner() else { return };
        for seed in [1u8, 7, 42, 99, 200] {
            for nonce in [0u64, 1, 0xFFFF_FFFF, 1 << 40, u64::MAX - 3] {
                let h = header_with_nonce(seed, nonce);
                let gpu = m.hash_on_gpu(&h, nonce);
                let cpu = blake3::hash(&h);
                assert_eq!(gpu, *cpu.as_bytes(), "seed={seed} nonce={nonce}");
            }
        }
    }

    #[test]
    fn gpu_search_finds_same_nonce_as_cpu_scan() {
        let Some(m) = miner() else { return };
        let zero_bits = 12u32;
        let base = 5000u64;
        let h = header_with_nonce(3, 0);
        // CPU reference scan.
        let mut expected = None;
        for n in base..base + 2_000_000 {
            let mut hh = h;
            hh[44..52].copy_from_slice(&n.to_le_bytes());
            let hash = blake3::hash(&hh);
            let lz = hash
                .as_bytes()
                .iter()
                .try_fold(0u32, |acc, &b| {
                    if b == 0 {
                        Ok(acc + 8)
                    } else {
                        Err(acc + b.leading_zeros())
                    }
                })
                .unwrap_or_else(|e| e);
            if lz >= zero_bits {
                expected = Some(n);
                break;
            }
        }
        let expected = expected.expect("reference scan found no nonce");
        let stop = AtomicBool::new(false);
        let got = m.search(&h, zero_bits, base, 2_000_000, 1 << 18, &stop);
        assert_eq!(got, Some(expected));
    }

    #[test]
    fn self_check_passes() {
        let Some(m) = miner() else { return };
        m.self_check().expect("self-check");
    }
}

#[cfg(test)]
mod select_tests {
    use super::*;

    fn dev(i: u32, name: &str) -> GpuDevice {
        GpuDevice {
            index: i,
            name: name.into(),
            backend: "Vulkan".into(),
            vendor: 0x10de,
            device: i,
        }
    }

    #[test]
    fn nothing_set_selects_every_usable_adapter() {
        let all = vec![dev(0, "A"), dev(1, "B")];
        assert_eq!(select_indices(&all, None, None).unwrap(), vec![0, 1]);
    }

    #[test]
    fn gpu_devices_picks_by_index_and_rejects_unknown_ones() {
        let all = vec![dev(0, "A"), dev(1, "B"), dev(2, "C")];
        assert_eq!(
            select_indices(&all, Some("2, 0"), None).unwrap(),
            vec![0, 2]
        );
        assert!(select_indices(&all, Some("3"), None).is_err());
        assert!(select_indices(&all, Some("x"), None).is_err());
        assert!(select_indices(&all, Some(" , "), None).is_err());
    }

    #[test]
    fn gpu_index_means_exactly_that_one() {
        let all = vec![dev(0, "A"), dev(1, "B")];
        assert_eq!(select_indices(&all, None, Some("1")).unwrap(), vec![1]);
        assert!(select_indices(&all, None, Some("2")).is_err());
    }

    // The GL view of a card carries the renderer string, not the product
    // name ("NVIDIA GeForce RTX 5090/PCIe/SSE2"), so a name match cannot
    // catch it. GL is never the backend to mine on when a real one exists.
    #[test]
    fn a_gl_view_of_a_card_is_dropped_when_a_real_backend_lists_it() {
        let vk = GpuDevice {
            index: 0,
            name: "NVIDIA GeForce RTX 5090".into(),
            backend: "Vulkan".into(),
            vendor: 0x10de,
            device: 0x2b85,
        };
        let gl = GpuDevice {
            index: 1,
            name: "NVIDIA GeForce RTX 5090/PCIe/SSE2".into(),
            backend: "Gl".into(),
            vendor: 0x10de,
            device: 0,
        };
        let kept = dedup_physical(vec![vk.clone(), gl.clone()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].backend, "Vulkan");
        // GL alone (no other backend usable) is still a device.
        assert_eq!(dedup_physical(vec![gl]).len(), 1);
    }

    fn nv(index: u32, name: &str, backend: &str, device: u32) -> GpuDevice {
        GpuDevice {
            index,
            name: name.into(),
            backend: backend.into(),
            vendor: 0x10de,
            device,
        }
    }

    // Wine's DX12 layer shows the one real card again under an invented
    // name and id ("GTX 470"): nothing matches it to its Vulkan twin but
    // the count. A later backend listing no more cards of a vendor than
    // the first one did adds no card.
    #[test]
    fn a_later_backend_with_no_more_cards_of_a_vendor_adds_none() {
        let list = vec![
            nv(0, "NVIDIA GeForce RTX 5090", "Vulkan", 0x2b85),
            nv(1, "NVIDIA GeForce GTX 470", "Dx12", 0x06cd),
        ];
        let kept = dedup_physical(list);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].name, "NVIDIA GeForce RTX 5090");
    }

    // Two real cards seen by both backends stay two.
    #[test]
    fn two_cards_under_two_backends_stay_two() {
        let list = vec![
            nv(0, "A", "Vulkan", 1),
            nv(1, "B", "Vulkan", 2),
            nv(2, "A", "Dx12", 1),
            nv(3, "B", "Dx12", 2),
        ];
        let kept = dedup_physical(list);
        assert_eq!(
            kept.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            ["A", "B"]
        );
        assert!(kept.iter().all(|d| d.backend == "Vulkan"));
    }

    // A card only DX12 can drive (a CMP card with no working Vulkan) is not
    // lost: DX12 lists more cards of the vendor than Vulkan did.
    #[test]
    fn a_card_only_a_later_backend_lists_is_kept() {
        let list = vec![
            nv(0, "A", "Vulkan", 1),
            nv(1, "A", "Dx12", 1),
            nv(2, "CMP 30HX", "Dx12", 3),
        ];
        let kept = dedup_physical(list);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert_eq!(kept[1].name, "CMP 30HX");
        assert_eq!(kept[1].index, 1);
    }

    // Another vendor under another backend is its own card.
    #[test]
    fn another_vendor_is_never_folded_away() {
        let mut intel = nv(1, "Intel Arc", "Dx12", 9);
        intel.vendor = 0x8086;
        let kept = dedup_physical(vec![nv(0, "A", "Vulkan", 1), intel]);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn duplicate_physical_adapters_across_backends_collapse_to_one() {
        let vk = GpuDevice {
            index: 0,
            name: "X".into(),
            backend: "Vulkan".into(),
            vendor: 1,
            device: 7,
        };
        let gl = GpuDevice {
            index: 1,
            name: "X".into(),
            backend: "Gl".into(),
            vendor: 1,
            device: 7,
        };
        let other = GpuDevice {
            index: 2,
            name: "Y".into(),
            backend: "Vulkan".into(),
            vendor: 1,
            device: 8,
        };
        let out = dedup_physical(vec![vk.clone(), gl, other]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], vk);
        assert_eq!(out[1].index, 1);
    }
}

#[cfg(test)]
mod pool_tests {
    use super::*;

    #[test]
    fn slots_start_apart_and_the_first_starts_at_base() {
        for slots in 1..=8usize {
            let bases: Vec<u64> = (0..slots)
                .map(|s| nonce_base_for_slot(1000, s, slots))
                .collect();
            assert_eq!(bases[0], 1000);
            for w in bases.windows(2) {
                assert!(
                    w[1].wrapping_sub(w[0]) >= u64::MAX / slots as u64 - 1,
                    "{bases:?}"
                );
            }
        }
    }

    /// Needs a GPU. Builds the same adapter twice and lets both search: one
    /// hit comes back and both devices did work.
    #[test]
    #[ignore]
    fn gpu_two_slots_share_one_hit() {
        let pool = GpuPool::build_for_test(2).expect("a GPU");
        assert_eq!(pool.live().len(), 2);
        let tip = AtomicU64::new(0);
        let progress = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        // difficulty 16 on an old parent: zero_bits 1, a hit within one dispatch.
        let hit = pool.attempt(
            1,
            &[0u8; 32],
            &[0u8; 32],
            16,
            0,
            Duration::from_secs(10),
            &tip,
            0,
            &progress,
            &stop,
        );
        assert!(hit.is_some());
        let snaps = pool.snapshots();
        assert_eq!(snaps.len(), 2);
        assert!(snaps.iter().all(|s| s.alive));
        assert!(snaps.iter().any(|s| s.hashes > 0));
    }

    /// Needs a GPU: a real search with no hit runs every device until the
    /// budget, each reporting its own hashes and rate.
    #[test]
    #[ignore]
    fn gpu_every_slot_reports_its_own_rate() {
        let pool = GpuPool::build_for_test(2).expect("a GPU");
        let tip = AtomicU64::new(0);
        let progress = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        // A far-future parent timestamp keeps difficulty huge: no hit.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let hit = pool.attempt(
            1,
            &[1u8; 32],
            &[2u8; 32],
            16 * 200,
            now,
            Duration::from_millis(1500),
            &tip,
            0,
            &progress,
            &stop,
        );
        assert!(hit.is_none());
        let snaps = pool.snapshots();
        assert!(
            snaps.iter().all(|s| s.hashes > 0 && s.hps > 0.0),
            "{snaps:?}"
        );
    }
}
