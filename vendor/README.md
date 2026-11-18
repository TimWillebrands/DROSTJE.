# Vendored crates

Each directory replaces a crates.io package through `[patch.crates-io]` in the root `Cargo.toml`. Changes inside are marked `DROSTJE patch`; upstream licence notices stay. Drop a patch only once the upstream release has the fix.

| Crate | Why |
|---|---|
| `zed-font-kit` | Keeps GPUI pinned while sharing Servo's FreeType ABI; only its FreeType dependency requirements differ. |
| `stylo_derive` | GPUI's `log`/`serde` features add a second `From<_> for fmt::Error`; the generated CSS result type is constrained instead of relying on a unique conversion. |
| `servo-webgpu` | `web_exposed_limits` clamps raw driver binding counts (524288 samplers on Vulkan, from which PixiJS sizes its batch shader) to WebGPU defaults. `WebGpuExternalImages::lock` never hands WebRender fewer bytes than a descriptor it may still upload, which panicked when a canvas shrank. Destroyed canvases keep their external image id, so a frame built before deletion locks as `Invalid`. Messages for a gone device are ignored. Per-document threads start via `start_webgpu_thread_for_document`. |
| `gpui-fast-web` | gpui-kit names the optional, wasm-only GPUI Fast backend, so Cargo resolves it even though DROSTJE. never builds it; 0.1.12 pins `unicode-properties = "=0.1.3"` while Servo needs `^0.1.4`, and the only line changed relaxes that pin. Newer GPUI Fast releases drop the pin but need ICU 2.3, which Servo's SpiderMonkey (`icu_capi = "=2.1.2"`) rules out; that same ICU pin keeps gpui-kit at 0.8.x (gpui-pre 0.3.10) until Servo moves. |
| `servo-constellation` | Upstream shares one WebGPU thread per site and never frees a navigated-away document's devices; after about ten, device creation panics the shared thread. This copy runs one WebGPU thread per document and exits it with the pipeline. A content process that exits while its pages are open reports those pages as crashed, and on Linux children re-execute `/proc/self/exe` so an update installed while DROSTJE runs cannot break them. |

`[profile.dev]` sets `debug = 0` because Servo's generated DOM bindings make debug symbols huge, and `naga` builds at `opt-level = 3` because Servo compiles page shaders synchronously and unoptimised naga stalls shader-heavy pages for minutes.
