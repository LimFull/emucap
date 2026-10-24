//! Azahar's Vulkan hardware renderer: the host plays the libretro Vulkan frontend.
//!
//! The host loads a Vulkan implementation (MoltenVK on macOS, staged next to the core), creates
//! the instance, and lets the core create the device through the context negotiation interface.
//! The core renders both screens into one output image and hands it over with `set_image` after
//! its commands are on the queue (patch `0003`); the host copies that image back only when a
//! screenshot asks for it, so frames are never stalled for readback.
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Condvar;

use ash::vk;

use super::*;

pub(super) const RETRO_HW_CONTEXT_VULKAN: u32 = 6;
const RETRO_HW_RENDER_INTERFACE_VULKAN_VERSION: u32 = 5;
/// `RETRO_HW_FRAME_BUFFER_VALID` passed as the video callback's data pointer.
pub(super) const HW_FRAME_BUFFER_VALID: usize = usize::MAX;
/// MoltenVK staged by `build.sh` and copied next to the core by launch.
pub(super) const STAGED_LIBRARY: &str = "libMoltenVK.dylib";

#[repr(C)]
pub(super) struct RetroHwRenderCallback {
    context_type: u32,
    context_reset: Option<unsafe extern "C" fn()>,
    get_current_framebuffer: *const c_void,
    get_proc_address: *const c_void,
    depth: bool,
    stencil: bool,
    bottom_left_origin: bool,
    version_major: u32,
    version_minor: u32,
    cache_context: bool,
    context_destroy: Option<unsafe extern "C" fn()>,
    debug_context: bool,
}

#[repr(C)]
#[derive(Default)]
struct RetroVulkanContext {
    gpu: vk::PhysicalDevice,
    device: vk::Device,
    queue: vk::Queue,
    queue_family_index: u32,
    presentation_queue: vk::Queue,
    presentation_queue_family_index: u32,
}

type CreateDevice = unsafe extern "C" fn(
    *mut RetroVulkanContext,
    vk::Instance,
    vk::PhysicalDevice,
    vk::SurfaceKHR,
    vk::PFN_vkGetInstanceProcAddr,
    *const *const c_char,
    u32,
    *const *const c_char,
    u32,
    *const vk::PhysicalDeviceFeatures,
) -> bool;

/// The leading, version-1 fields of `retro_hw_render_context_negotiation_interface_vulkan`.
#[repr(C)]
pub(super) struct Negotiation {
    interface_type: u32,
    interface_version: u32,
    get_application_info: Option<unsafe extern "C" fn() -> *const vk::ApplicationInfo<'static>>,
    create_device: Option<CreateDevice>,
}

#[repr(C)]
struct RetroVulkanImage {
    image_view: vk::ImageView,
    image_layout: vk::ImageLayout,
    create_info: vk::ImageViewCreateInfo<'static>,
}

type SetImage =
    unsafe extern "C" fn(*mut c_void, *const RetroVulkanImage, u32, *const vk::Semaphore, u32);

#[repr(C)]
struct Interface {
    interface_type: u32,
    interface_version: u32,
    handle: *mut c_void,
    instance: vk::Instance,
    gpu: vk::PhysicalDevice,
    device: vk::Device,
    get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
    get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    queue: vk::Queue,
    queue_index: u32,
    set_image: SetImage,
    get_sync_index: unsafe extern "C" fn(*mut c_void) -> u32,
    get_sync_index_mask: unsafe extern "C" fn(*mut c_void) -> u32,
    set_command_buffers: unsafe extern "C" fn(*mut c_void, u32, *const vk::CommandBuffer),
    wait_sync_index: unsafe extern "C" fn(*mut c_void),
    lock_queue: unsafe extern "C" fn(*mut c_void),
    unlock_queue: unsafe extern "C" fn(*mut c_void),
    set_signal_semaphore: unsafe extern "C" fn(*mut c_void, vk::Semaphore),
}

#[derive(Clone, Copy)]
struct Readback {
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *const u8,
    size: u64,
}

struct Context {
    _entry: ash::Entry,
    _instance: ash::Instance,
    device: ash::Device,
    queue: vk::Queue,
    family: u32,
    memory: vk::PhysicalDeviceMemoryProperties,
    readback: Option<Readback>,
}

#[derive(Default)]
struct State {
    entry: Option<ash::Entry>,
    context_reset: Option<unsafe extern "C" fn()>,
    negotiation: Option<&'static Negotiation>,
    context: Option<Context>,
    /// Image, layout and format of the last `set_image`.
    image: Option<(vk::Image, vk::ImageLayout, vk::Format)>,
}

// Vulkan handles and core-owned statics are used only under the lock.
unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static INTERFACE: AtomicPtr<Interface> = AtomicPtr::new(ptr::null_mut());
static QUEUE_BUSY: Mutex<bool> = Mutex::new(false);
static QUEUE_FREE: Condvar = Condvar::new();

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    f(STATE
        .lock()
        .unwrap_or_else(|value| value.into_inner())
        .get_or_insert_with(State::default))
}

fn vk_error(what: &str) -> impl Fn(vk::Result) -> AzaharError + '_ {
    move |result| AzaharError::Core(format!("Vulkan {what}: {result}"))
}

pub(super) fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Renderer requested by `EMUCAP_AZAHAR_RENDERER`: `vulkan`, `software`, or unset for Vulkan
/// when a Vulkan implementation loads and the software renderer otherwise.
pub(super) fn select(core_path: &Path) -> AzaharResult<&'static str> {
    let requested = std::env::var("EMUCAP_AZAHAR_RENDERER").unwrap_or_default();
    match requested.as_str() {
        "software" => return Ok("Software"),
        "" | "vulkan" => {}
        other => {
            return Err(AzaharError::BadParams(format!(
                "EMUCAP_AZAHAR_RENDERER must be vulkan or software, got {other:?}"
            )))
        }
    }
    let staged = core_path.with_file_name(STAGED_LIBRARY);
    let entry = unsafe {
        if staged.is_file() {
            ash::Entry::load_from(&staged)
        } else {
            ash::Entry::load()
        }
    };
    match entry {
        Ok(entry) => {
            with(|state| state.entry = Some(entry));
            ACTIVE.store(true, Ordering::Release);
            Ok("Vulkan")
        }
        Err(_) if requested.is_empty() => Ok("Software"),
        Err(error) => Err(AzaharError::Dynamic(format!(
            "Vulkan implementation not found ({error}); set EMUCAP_AZAHAR_RENDERER=software"
        ))),
    }
}

/// `RETRO_ENVIRONMENT_SET_HW_RENDER`.
pub(super) unsafe fn set_hw_render(callback: *mut RetroHwRenderCallback) -> bool {
    let callback = &*callback;
    if !active() || callback.context_type != RETRO_HW_CONTEXT_VULKAN {
        return false;
    }
    with(|state| state.context_reset = callback.context_reset);
    true
}

/// `RETRO_ENVIRONMENT_SET_HW_RENDER_CONTEXT_NEGOTIATION_INTERFACE`.
pub(super) unsafe fn set_negotiation(interface: *const Negotiation) -> bool {
    if !active() || (*interface).interface_type != 0 {
        return false;
    }
    with(|state| state.negotiation = Some(&*interface));
    true
}

/// `RETRO_ENVIRONMENT_GET_HW_RENDER_INTERFACE`.
pub(super) fn interface() -> *const c_void {
    INTERFACE.load(Ordering::Acquire).cast()
}

/// Creates the instance and the core's device, then lets the core finish loading the title.
pub(super) fn start() -> AzaharResult<()> {
    let (context_reset, interface) = with(|state| -> AzaharResult<_> {
        let entry = state.entry.take().ok_or_else(|| {
            AzaharError::BadState("Vulkan implementation was not loaded".into())
        })?;
        let negotiation = state.negotiation.ok_or_else(|| {
            AzaharError::Core("the core did not offer Vulkan context negotiation".into())
        })?;
        let context_reset = state
            .context_reset
            .ok_or_else(|| AzaharError::Core("the core did not request a Vulkan context".into()))?;
        let create_device = negotiation
            .create_device
            .ok_or_else(|| AzaharError::Core("the core cannot create a Vulkan device".into()))?;
        let default_app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
        let app = match negotiation.get_application_info {
            Some(info) => unsafe { info().as_ref() }.unwrap_or(&default_app),
            None => &default_app,
        };
        // A system loader hides MoltenVK unless portability enumeration is requested.
        let portability = c"VK_KHR_portability_enumeration";
        let has_portability = unsafe { entry.enumerate_instance_extension_properties(None) }
            .unwrap_or_default()
            .iter()
            .any(|e| e.extension_name_as_c_str() == Ok(portability));
        let extensions = [portability.as_ptr()];
        let mut create_info = vk::InstanceCreateInfo::default().application_info(app);
        if has_portability {
            create_info = create_info
                .enabled_extension_names(&extensions)
                .flags(vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR);
        }
        let instance = unsafe { entry.create_instance(&create_info, None) }
            .map_err(vk_error("instance creation"))?;
        let gpu = *unsafe { instance.enumerate_physical_devices() }
            .map_err(vk_error("device enumeration"))?
            .first()
            .ok_or_else(|| AzaharError::Core("no Vulkan device".into()))?;
        let get_instance_proc_addr = entry.static_fn().get_instance_proc_addr;
        let mut created = RetroVulkanContext::default();
        if !unsafe {
            create_device(
                &mut created,
                instance.handle(),
                gpu,
                vk::SurfaceKHR::null(),
                get_instance_proc_addr,
                ptr::null(),
                0,
                ptr::null(),
                0,
                ptr::null(),
            )
        } {
            return Err(AzaharError::Core("the core failed to create its Vulkan device".into()));
        }
        let device = unsafe { ash::Device::load(instance.fp_v1_0(), created.device) };
        let interface = Box::into_raw(Box::new(Interface {
            interface_type: 0,
            interface_version: RETRO_HW_RENDER_INTERFACE_VULKAN_VERSION,
            handle: ptr::null_mut(),
            instance: instance.handle(),
            gpu: created.gpu,
            device: created.device,
            get_device_proc_addr: instance.fp_v1_0().get_device_proc_addr,
            get_instance_proc_addr,
            queue: created.queue,
            queue_index: created.queue_family_index,
            set_image,
            get_sync_index,
            get_sync_index_mask,
            set_command_buffers,
            wait_sync_index,
            lock_queue,
            unlock_queue,
            set_signal_semaphore,
        }));
        state.context = Some(Context {
            memory: unsafe { instance.get_physical_device_memory_properties(created.gpu) },
            _entry: entry,
            _instance: instance,
            device,
            queue: created.queue,
            family: created.queue_family_index,
            readback: None,
        });
        Ok((context_reset, interface))
    })?;
    // ponytail: the instance, device and interface live until the process exits.
    INTERFACE.store(interface, Ordering::Release);
    unsafe { context_reset() };
    Ok(())
}

unsafe extern "C" fn set_image(
    _handle: *mut c_void,
    image: *const RetroVulkanImage,
    _semaphores: u32,
    _semaphore_list: *const vk::Semaphore,
    _queue_family: u32,
) {
    let image = image.as_ref().map(|image| {
        (image.create_info.image, image.image_layout, image.create_info.format)
    });
    with(|state| state.image = image);
}
unsafe extern "C" fn get_sync_index(_handle: *mut c_void) -> u32 {
    0
}
unsafe extern "C" fn get_sync_index_mask(_handle: *mut c_void) -> u32 {
    1
}
// The core records and submits its own command buffers; it never hands any to the frontend.
unsafe extern "C" fn set_command_buffers(_: *mut c_void, _: u32, _: *const vk::CommandBuffer) {}
unsafe extern "C" fn wait_sync_index(_handle: *mut c_void) {}
unsafe extern "C" fn set_signal_semaphore(_handle: *mut c_void, _semaphore: vk::Semaphore) {}
unsafe extern "C" fn lock_queue(_handle: *mut c_void) {
    let mut busy = QUEUE_BUSY.lock().unwrap_or_else(|value| value.into_inner());
    while *busy {
        busy = QUEUE_FREE.wait(busy).unwrap_or_else(|value| value.into_inner());
    }
    *busy = true;
}
unsafe extern "C" fn unlock_queue(_handle: *mut c_void) {
    *QUEUE_BUSY.lock().unwrap_or_else(|value| value.into_inner()) = false;
    QUEUE_FREE.notify_one();
}

/// The last presented image as XRGB8888 rows of `width * 4` bytes.
pub(super) fn read_frame(width: u32, height: u32) -> AzaharResult<Vec<u8>> {
    with(|state| {
        let (image, layout, format) = state.image.ok_or_else(|| {
            AzaharError::BadState("Azahar has not presented a Vulkan frame".into())
        })?;
        let context = state
            .context
            .as_mut()
            .ok_or_else(|| AzaharError::BadState("Vulkan context is not running".into()))?;
        let size = u64::from(width) * u64::from(height) * 4;
        let readback = context.readback(size)?;
        let device = &context.device;
        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        let to_transfer = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .old_layout(layout)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range);
        let back = to_transfer
            .src_access_mask(vk::AccessFlags::TRANSFER_READ)
            .dst_access_mask(vk::AccessFlags::SHADER_READ)
            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .new_layout(layout);
        let to_host = vk::MemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::HOST_READ);
        let copy = vk::BufferImageCopy::default()
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .layer_count(1),
            )
            .image_extent(vk::Extent3D { width, height, depth: 1 });
        unsafe {
            device
                .begin_command_buffer(
                    readback.cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(vk_error("begin readback"))?;
            device.cmd_pipeline_barrier(
                readback.cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_transfer],
            );
            device.cmd_copy_image_to_buffer(
                readback.cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                readback.buffer,
                &[copy],
            );
            device.cmd_pipeline_barrier(
                readback.cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS | vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[to_host],
                &[],
                &[back],
            );
            device
                .end_command_buffer(readback.cmd)
                .map_err(vk_error("end readback"))?;
            let cmds = [readback.cmd];
            let submit = vk::SubmitInfo::default().command_buffers(&cmds);
            lock_queue(ptr::null_mut());
            let submitted = device.queue_submit(context.queue, &[submit], readback.fence);
            unlock_queue(ptr::null_mut());
            submitted.map_err(vk_error("readback submit"))?;
            device
                .wait_for_fences(&[readback.fence], true, 5_000_000_000)
                .map_err(vk_error("readback wait"))?;
            device
                .reset_fences(&[readback.fence])
                .map_err(vk_error("readback fence reset"))?;
            let pixels = std::slice::from_raw_parts(readback.mapped, size as usize);
            match format {
                vk::Format::B8G8R8A8_UNORM => Ok(pixels.to_vec()),
                vk::Format::R8G8B8A8_UNORM => Ok(pixels
                    .chunks_exact(4)
                    .flat_map(|p| [p[2], p[1], p[0], 0xff])
                    .collect()),
                other => Err(AzaharError::Core(format!(
                    "unsupported Vulkan output format {other:?}"
                ))),
            }
        }
    })
}

impl Context {
    /// A mapped host buffer of at least `size` bytes with its command buffer and fence.
    fn readback(&mut self, size: u64) -> AzaharResult<Readback> {
        if let Some(readback) = self.readback.filter(|r| r.size >= size) {
            return Ok(readback);
        }
        let device = &self.device;
        unsafe {
            if let Some(old) = self.readback.take() {
                device.destroy_buffer(old.buffer, None);
                device.free_memory(old.memory, None);
                device.destroy_fence(old.fence, None);
                device.destroy_command_pool(old.pool, None);
            }
            let pool = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                        .queue_family_index(self.family),
                    None,
                )
                .map_err(vk_error("command pool"))?;
            let cmd = device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .command_buffer_count(1),
                )
                .map_err(vk_error("command buffer"))?[0];
            let fence = device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(vk_error("fence"))?;
            let buffer = device
                .create_buffer(
                    &vk::BufferCreateInfo::default()
                        .size(size)
                        .usage(vk::BufferUsageFlags::TRANSFER_DST),
                    None,
                )
                .map_err(vk_error("readback buffer"))?;
            let requirements = device.get_buffer_memory_requirements(buffer);
            let wanted =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let memory_type = (0..self.memory.memory_type_count)
                .find(|&i| {
                    requirements.memory_type_bits & (1 << i) != 0
                        && self.memory.memory_types[i as usize]
                            .property_flags
                            .contains(wanted)
                })
                .ok_or_else(|| AzaharError::Core("no host-visible Vulkan memory".into()))?;
            let memory = device
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(requirements.size)
                        .memory_type_index(memory_type),
                    None,
                )
                .map_err(vk_error("readback memory"))?;
            device
                .bind_buffer_memory(buffer, memory, 0)
                .map_err(vk_error("readback bind"))?;
            let mapped = device
                .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
                .map_err(vk_error("readback map"))?
                .cast::<u8>()
                .cast_const();
            self.readback = Some(Readback {
                pool,
                cmd,
                fence,
                buffer,
                memory,
                mapped,
                size,
            });
        }
        Ok(self.readback.expect("created"))
    }
}
