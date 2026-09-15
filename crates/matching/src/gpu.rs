//! GPU matching with wgpu.
//!
//! A port of upstream template-matching's `TemplateMatcher` to wgpu 30.
//! Since this is native-only, upstream's `futures-intrusive` is replaced with
//! `std::sync::mpsc`.

use std::borrow::Cow;
use std::fmt;
use std::mem::size_of;

use wgpu::util::DeviceExt;

use crate::prepare::{FLAT_TEMPLATE_EPS, center_input, prepare_template};
use crate::{Image, MatchMethod, result_size};

/// Workgroup X/Y size. Must match `@workgroup_size` in `shaders/matching.wgsl`.
const WORKGROUP_SIZE: u32 = 16;

/// Upper bound on the input tile held in shared memory (count of f32).
/// Must match `TILE_CAPACITY` in `shaders/matching.wgsl`.
const TILE_CAPACITY: u32 = 2304;

/// Whether to use the tiled shader.
///
/// It computes exactly the same values as the naive variant (the golden tests
/// confirm bit-identical output). Only the speed differs.
///
/// # Why the default is `Never`
///
/// **It measured no faster.** Ratios on an RTX 4080 (naive → tiled):
///
/// | template | 1920x1080 | 3840x2160 |
/// |---|---|---|
/// | 16px | 1.03x | 0.97x |
/// | 32px | 0.98x | 0.82x |
///
/// At 34px and above the input tile does not fit in shared memory, so tiling is
/// not even an option there.
///
/// The expectation failed because the naive variant was already getting plenty
/// of bandwidth from cache. At 4K / 32px it reaches an effective 1.6 TB/s,
/// which is evidence that L1/L2 are doing the work. Explicitly staging through
/// shared memory adds barriers and lowers occupancy, cancelling out the gain.
///
/// The implementation is kept because the conclusion could differ on a GPU with
/// weaker caches (integrated GPUs and the like). Switch to
/// [`Tiling::WhenItFits`] to measure again.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Tiling {
    /// Only ever use the naive variant. **The default.**
    #[default]
    Never,
    /// Use the tiled variant when it fits in shared memory.
    WhenItFits,
}

/// Whether the input tile fits in shared memory.
///
/// A 16x16 output tile needs `(16+tw-1) x (16+th-1)` of input. A large template
/// does not fit, and we fall back to the naive variant in that case.
pub(crate) fn tile_fits(template: (u32, u32)) -> bool {
    let w = WORKGROUP_SIZE + template.0.saturating_sub(1);
    let h = WORKGROUP_SIZE + template.1.saturating_sub(1);
    w.checked_mul(h).is_some_and(|len| len <= TILE_CAPACITY)
}

#[derive(Debug)]
pub enum MatchError {
    /// No usable adapter (no GPU, no driver installed, and so on).
    NoAdapter,
    /// The only adapter is a software rasterizer (WARP on Windows, lavapipe on
    /// Linux). Refused unless `MEKIKI_ALLOW_SOFTWARE_GPU` is set: they are
    /// slower than the CPU path and WARP crashed the process on GitHub's
    /// Windows runners (STATUS_ACCESS_VIOLATION, 2026-09).
    SoftwareAdapter(String),
    /// Device creation failed.
    DeviceRequest(String),
    /// The template is larger than the input, or a size is 0.
    InvalidSize {
        input: (u32, u32),
        template: (u32, u32),
    },
    /// Mapping the result buffer failed.
    Map(String),
}

impl fmt::Display for MatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAdapter => write!(f, "no usable GPU adapter found"),
            Self::SoftwareAdapter(name) => write!(
                f,
                "the only GPU adapter is a software rasterizer ({name}); set MEKIKI_ALLOW_SOFTWARE_GPU=1 to use it anyway"
            ),
            Self::DeviceRequest(e) => write!(f, "failed to create the wgpu device: {e}"),
            Self::InvalidSize { input, template } => write!(
                f,
                "a {}x{} template cannot be used against a {}x{} input",
                template.0, template.1, input.0, input.1
            ),
            Self::Map(e) => write!(f, "failed to map the result buffer: {e}"),
        }
    }
}

impl std::error::Error for MatchError {}

/// Upload data into a storage buffer, reusing the existing one where possible.
///
/// Returns whether the buffer was recreated. If it was, the bind group has to
/// be rebuilt too.
fn upload_storage(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    slot: &mut Option<wgpu::Buffer>,
    size_unchanged: bool,
    label: &'static str,
    data: &[f32],
) -> bool {
    if size_unchanged {
        if let Some(buffer) = slot.as_ref() {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(data));
            return false;
        }
    }

    *slot = Some(
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytemuck::cast_slice(data),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        }),
    );
    true
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct ShaderUniforms {
    input_width: u32,
    input_height: u32,
    template_width: u32,
    template_height: u32,
    result_width: u32,
    result_height: u32,
    /// One half of the ZMD denominator, `sum((T - mean(T))^2)`. The square of
    /// `PreparedTemplate::norm`.
    template_norm2: f32,
    inv_area: f32,
}

// A struct in the uniform address space must have a size that is a multiple of 16.
const _: () = assert!(size_of::<ShaderUniforms>() % 16 == 0);

/// Runs template matching on the GPU.
///
/// Construction is expensive (adapter, device and pipeline initialisation), so
/// reuse one instead of creating it per call. Buffers are also reused as long
/// as the input size does not change.
pub struct TemplateMatcher {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter_info: wgpu::AdapterInfo,

    bind_group_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,

    /// Pipeline keyed by entry point name. The same method with and without
    /// tiling are two different pipelines.
    pipeline: Option<(&'static str, wgpu::ComputePipeline)>,
    tiling: Tiling,

    uniform_buffer: wgpu::Buffer,
    input_buffer: Option<wgpu::Buffer>,
    template_buffer: Option<wgpu::Buffer>,
    result_buffer: Option<wgpu::Buffer>,
    staging_buffer: Option<wgpu::Buffer>,
    bind_group: Option<wgpu::BindGroup>,

    last_input_size: (u32, u32),
    last_template_size: (u32, u32),
    last_result_size: (u32, u32),
}

impl TemplateMatcher {
    /// Initialise with the default adapter (preferring high performance).
    pub fn new() -> Result<Self, MatchError> {
        pollster::block_on(Self::new_async())
    }

    pub async fn new_async() -> Result<Self, MatchError> {
        let instance = wgpu::Instance::default();

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .map_err(|_| MatchError::NoAdapter)?;

        let adapter_info = adapter.get_info();
        log::info!(
            "Mekiki matching: {} ({:?}, {:?})",
            adapter_info.name,
            adapter_info.backend,
            adapter_info.device_type
        );

        // A software rasterizer is not a GPU for our purposes: the CPU path is
        // faster, and WARP has crashed the process outright. CI opts in on
        // Linux, where lavapipe is used to exercise the shader.
        if adapter_info.device_type == wgpu::DeviceType::Cpu
            && std::env::var_os("MEKIKI_ALLOW_SOFTWARE_GPU").is_none()
        {
            return Err(MatchError::SoftwareAdapter(adapter_info.name));
        }

        // A 4K f32 image is about 33MB. The default limits allow storage
        // buffers up to 128MB, which is enough, but we request the adapter's
        // own limits to leave headroom.
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("mekiki-matching"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| MatchError::DeviceRequest(e.to_string()))?;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("matching.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                "../shaders/matching.wgsl"
            ))),
        });

        let storage_entry = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("matching-bgl"),
            entries: &[
                storage_entry(0, true),  // input
                storage_entry(1, true),  // template
                storage_entry(2, false), // result
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("matching-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniform_buffer"),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            size: size_of::<ShaderUniforms>() as u64,
            mapped_at_creation: false,
        });

        Ok(Self {
            device,
            queue,
            adapter_info,
            bind_group_layout,
            pipeline_layout,
            shader,
            pipeline: None,
            tiling: Tiling::default(),
            uniform_buffer,
            input_buffer: None,
            template_buffer: None,
            result_buffer: None,
            staging_buffer: None,
            bind_group: None,
            last_input_size: (0, 0),
            last_template_size: (0, 0),
            last_result_size: (0, 0),
        })
    }

    /// Information about the adapter in use. Public so benchmarks can report it.
    pub fn adapter_info(&self) -> &wgpu::AdapterInfo {
        &self.adapter_info
    }

    /// Scan the template across the input and return the score at each position.
    ///
    /// The result is a `(W - w + 1) x (H - h + 1)` score map.
    pub fn match_template(
        &mut self,
        input: &Image<'_>,
        template: &Image<'_>,
        method: MatchMethod,
    ) -> Result<Image<'static>, MatchError> {
        let input_size = (input.width, input.height);
        let template_size = (template.width, template.height);
        let (result_width, result_height) =
            result_size(input_size, template_size).ok_or(MatchError::InvalidSize {
                input: input_size,
                template: template_size,
            })?;

        let prepared = prepare_template(template, method);

        // A uniform template is the only case where the ZMD denominator (tn2)
        // is 0. The layer above (the load-time check in Pattern) rejects those
        // first; this is a defence for callers using the matcher directly, so
        // we skip the dispatch and return an all-zero map. "A uniform template
        // matches nothing" is the correct answer for UI search (OpenCV fills
        // the map with 1.0, but that compatibility requirement has been dropped).
        if method == MatchMethod::ZeroMeanDice && prepared.norm < FLAT_TEMPLATE_EPS {
            log::debug!("uniform template: filling the score map with 0.0");
            return Ok(Image::new(
                vec![0.0f32; (result_width as usize) * (result_height as usize)],
                result_width,
                result_height,
            ));
        }

        let entry = self.entry_point_for(method, template_size);
        self.ensure_pipeline(entry);

        // Only ZMD centres the input on zero (result unchanged, just less
        // cancellation).
        let centered;
        let input_data: &[f32] = if method == MatchMethod::ZeroMeanDice {
            centered = center_input(input);
            &centered
        } else {
            &input.data
        };

        // If the size has not changed, replace the contents instead of
        // recreating the buffer. This matters when running screen capture in a
        // loop.
        let mut buffers_changed = upload_storage(
            &self.device,
            &self.queue,
            &mut self.input_buffer,
            self.last_input_size == input_size,
            "input_buffer",
            input_data,
        );
        self.last_input_size = input_size;

        buffers_changed |= upload_storage(
            &self.device,
            &self.queue,
            &mut self.template_buffer,
            self.last_template_size == template_size,
            "template_buffer",
            &prepared.data,
        );
        self.last_template_size = template_size;

        // Write the uniforms back every time. Upstream only wrote them back
        // when the template size changed, so input_width went stale whenever
        // only the input size changed.
        let area = (template_size.0 as f32) * (template_size.1 as f32);
        self.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&ShaderUniforms {
                input_width: input.width,
                input_height: input.height,
                template_width: template.width,
                template_height: template.height,
                result_width,
                result_height,
                template_norm2: prepared.norm * prepared.norm,
                inv_area: 1.0 / area,
            }),
        );

        let result_len = (result_width as usize) * (result_height as usize);
        let result_buf_size = (result_len * size_of::<f32>()) as u64;

        if buffers_changed || self.last_result_size != (result_width, result_height) {
            self.last_result_size = (result_width, result_height);

            self.result_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("result_buffer"),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                size: result_buf_size,
                mapped_at_creation: false,
            }));

            self.staging_buffer = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("staging_buffer"),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                size: result_buf_size,
                mapped_at_creation: false,
            }));

            self.bind_group = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("matching-bind-group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.input_buffer.as_ref().unwrap().as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.template_buffer.as_ref().unwrap().as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.result_buffer.as_ref().unwrap().as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                ],
            }));
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("matching-encoder"),
            });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("matching-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline.as_ref().unwrap().1);
            pass.set_bind_group(0, self.bind_group.as_ref().unwrap(), &[]);
            pass.dispatch_workgroups(
                result_width.div_ceil(WORKGROUP_SIZE),
                result_height.div_ceil(WORKGROUP_SIZE),
                1,
            );
        }

        encoder.copy_buffer_to_buffer(
            self.result_buffer.as_ref().unwrap(),
            0,
            self.staging_buffer.as_ref().unwrap(),
            0,
            result_buf_size,
        );

        self.queue.submit(std::iter::once(encoder.finish()));

        let data = self.read_staging(result_len)?;
        Ok(Image::new(data, result_width, result_height))
    }

    /// Configure whether the tiled variant may be used.
    pub fn set_tiling(&mut self, tiling: Tiling) {
        self.tiling = tiling;
    }

    pub fn tiling(&self) -> Tiling {
        self.tiling
    }

    /// The entry point actually used for this method and template size.
    fn entry_point_for(&self, method: MatchMethod, template: (u32, u32)) -> &'static str {
        let tiled = match self.tiling {
            Tiling::Never => false,
            Tiling::WhenItFits => tile_fits(template),
        };
        if tiled && method == MatchMethod::ZeroMeanDice {
            "main_zmd_tiled"
        } else {
            method.entry_point()
        }
    }

    fn ensure_pipeline(&mut self, entry: &'static str) {
        if matches!(&self.pipeline, Some((e, _)) if *e == entry) {
            return;
        }
        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&self.pipeline_layout),
                module: &self.shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            });
        self.pipeline = Some((entry, pipeline));
    }

    fn read_staging(&self, len: usize) -> Result<Vec<f32>, MatchError> {
        let staging = self.staging_buffer.as_ref().unwrap();
        let slice = staging.slice(..);

        // On native, the callback has already run by the time
        // PollType::wait_indefinitely() returns, so a std sync channel is
        // enough (upstream's futures-intrusive is not needed).
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });

        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| MatchError::Map(e.to_string()))?;

        rx.recv()
            .map_err(|e| MatchError::Map(e.to_string()))?
            .map_err(|e| MatchError::Map(e.to_string()))?;

        let result = {
            let view = slice
                .get_mapped_range()
                .map_err(|e| MatchError::Map(e.to_string()))?;
            let floats: &[f32] = bytemuck::cast_slice(&view);
            debug_assert_eq!(floats.len(), len);
            floats.to_vec()
        };
        staging.unmap();

        Ok(result)
    }
}
