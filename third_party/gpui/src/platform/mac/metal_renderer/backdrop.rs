use crate::{BackdropFilter, DevicePixels, Size};

pub(super) struct BackdropRenderer {
    pipeline: metal::RenderPipelineState,
    textures: Option<BackdropTextures>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bounds, ContentMask, Corners, ScaledPixels, Shadow, hsla, point, rgba, size};

    #[test]
    fn backdrop_should_filter_pixels_preserve_alpha_and_respect_rounded_clipping() {
        let device = metal::Device::system_default().expect("native Metal device required");
        #[cfg(not(feature = "runtime_shaders"))]
        let library = device
            .new_library_with_data(super::super::SHADERS_METALLIB)
            .unwrap();
        #[cfg(feature = "runtime_shaders")]
        let library = device
            .new_library_with_source(
                super::super::SHADERS_SOURCE_FILE,
                &metal::CompileOptions::new(),
            )
            .unwrap();
        let mut renderer = BackdropRenderer::new(&device, &library);
        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_width(96);
        descriptor.set_height(96);
        descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        descriptor.set_storage_mode(if device.has_unified_memory() {
            metal::MTLStorageMode::Shared
        } else {
            metal::MTLStorageMode::Managed
        });
        descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        let target = device.new_texture(&descriptor);
        let region = metal::MTLRegion::new_2d(0, 0, 96, 96);
        let mut original = vec![0u8; 96 * 96 * 4];
        for y in 0..96 {
            for x in 0..96 {
                let value = if x % 2 == 0 { 0 } else { 128 };
                original[(y * 96 + x) * 4..(y * 96 + x + 1) * 4]
                    .copy_from_slice(&[value, value, value, 128]);
            }
        }
        target.replace_region(region, 0, original.as_ptr().cast(), 96 * 4);
        let bounds = Bounds::new(
            point(ScaledPixels(16.0), ScaledPixels(16.0)),
            size(ScaledPixels(64.0), ScaledPixels(64.0)),
        );
        let filter = BackdropFilter {
            bounds,
            content_mask: ContentMask {
                bounds: Bounds::new(bounds.origin, size(ScaledPixels(48.0), ScaledPixels(64.0))),
            },
            corner_radii: Corners::all(ScaledPixels(16.0)),
            radius: ScaledPixels(8.0),
            opacity: 1.0,
            ..Default::default()
        };
        let queue = device.new_command_queue();
        let commands = queue.new_command_buffer();
        renderer.encode(
            &device,
            commands,
            &target,
            &filter,
            size(DevicePixels(96), DevicePixels(96)),
        );
        if !device.has_unified_memory() {
            let sync = commands.new_blit_command_encoder();
            sync.synchronize_resource(&target);
            sync.end_encoding();
        }
        commands.commit();
        commands.wait_until_completed();
        assert_eq!(commands.status(), metal::MTLCommandBufferStatus::Completed);
        let mut output = vec![0u8; original.len()];
        target.get_bytes(output.as_mut_ptr().cast(), 96 * 4, region, 0);
        let pixel = |x, y| &output[(y * 96 + x) * 4..(y * 96 + x + 1) * 4];
        assert!(
            (56..=72).contains(&pixel(40, 40)[0]),
            "the alternating input must blur to its mean, got {:?}",
            pixel(40, 40)
        );
        assert!(
            pixel(40, 40)[0].abs_diff(pixel(41, 40)[0]) <= 3,
            "sharp input stripes must disappear"
        );
        assert_eq!(
            pixel(40, 40)[3],
            128,
            "filtering must not accumulate destination alpha"
        );
        for (x, y) in [(4, 40), (16, 16), (70, 40)] {
            assert_eq!(
                pixel(x, y),
                &original[(y * 96 + x) * 4..(y * 96 + x + 1) * 4],
                "outside, rounded-corner and masked pixels must remain unchanged"
            );
        }

        // Reusing scratch textures must capture the new target, not a previous frame.
        original
            .chunks_exact_mut(4)
            .for_each(|pixel| pixel.copy_from_slice(&[16, 32, 64, 128]));
        target.replace_region(region, 0, original.as_ptr().cast(), 96 * 4);
        let commands = queue.new_command_buffer();
        renderer.encode(
            &device,
            commands,
            &target,
            &filter,
            size(DevicePixels(96), DevicePixels(96)),
        );
        if !device.has_unified_memory() {
            let sync = commands.new_blit_command_encoder();
            sync.synchronize_resource(&target);
            sync.end_encoding();
        }
        commands.commit();
        commands.wait_until_completed();
        target.get_bytes(output.as_mut_ptr().cast(), 96 * 4, region, 0);
        assert_eq!(
            output, original,
            "a constant fresh backdrop must stay constant"
        );
    }

    #[test]
    fn backdrop_tone_should_preserve_alpha_and_be_idempotent_without_blur() {
        let device = metal::Device::system_default().expect("native Metal device required");
        #[cfg(not(feature = "runtime_shaders"))]
        let library = device
            .new_library_with_data(super::super::SHADERS_METALLIB)
            .unwrap();
        #[cfg(feature = "runtime_shaders")]
        let library = device
            .new_library_with_source(
                super::super::SHADERS_SOURCE_FILE,
                &metal::CompileOptions::new(),
            )
            .unwrap();
        let mut renderer = BackdropRenderer::new(&device, &library);
        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_width(16);
        descriptor.set_height(16);
        descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        descriptor.set_storage_mode(if device.has_unified_memory() {
            metal::MTLStorageMode::Shared
        } else {
            metal::MTLStorageMode::Managed
        });
        descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        let target = device.new_texture(&descriptor);
        let region = metal::MTLRegion::new_2d(0, 0, 16, 16);
        let samples = [
            [0, 0, 0, 255],
            [64, 64, 64, 255],
            [255, 255, 255, 255],
            [128, 128, 128, 128],
            [0, 0, 0, 0],
        ];
        let mut original = vec![0u8; 16 * 16 * 4];
        for (index, pixel) in original.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&samples[index % samples.len()]);
        }
        target.replace_region(region, 0, original.as_ptr().cast(), 16 * 4);
        let bounds = Bounds::new(
            point(ScaledPixels(0.0), ScaledPixels(0.0)),
            size(ScaledPixels(16.0), ScaledPixels(16.0)),
        );
        let filter = BackdropFilter {
            bounds,
            content_mask: ContentMask { bounds },
            corner_radii: Corners::default(),
            radius: ScaledPixels(0.0),
            opacity: 1.0,
            tone: rgba(0x202020b3),
            ..Default::default()
        };
        let queue = device.new_command_queue();
        let render = |renderer: &mut BackdropRenderer, target: &metal::TextureRef| {
            let commands = queue.new_command_buffer();
            renderer.encode(
                &device,
                commands,
                target,
                &filter,
                size(DevicePixels(16), DevicePixels(16)),
            );
            if !device.has_unified_memory() {
                let sync = commands.new_blit_command_encoder();
                sync.synchronize_resource(target);
                sync.end_encoding();
            }
            commands.commit();
            commands.wait_until_completed();
            assert_eq!(commands.status(), metal::MTLCommandBufferStatus::Completed);
        };
        render(&mut renderer, &target);
        let mut output = vec![0u8; original.len()];
        target.get_bytes(output.as_mut_ptr().cast(), 16 * 4, region, 0);
        let pixel = |index: usize| &output[index * 4..(index + 1) * 4];
        for channel in 0..3 {
            assert!((21..=24).contains(&pixel(0)[channel]));
            assert!((62..=66).contains(&pixel(1)[channel]));
            assert!((97..=100).contains(&pixel(2)[channel]));
            assert!((48..=51).contains(&pixel(3)[channel]));
            assert_eq!(pixel(4)[channel], 0);
        }
        assert_eq!(pixel(0)[3], 255);
        assert_eq!(pixel(1)[3], 255);
        assert_eq!(pixel(2)[3], 255);
        assert_eq!(pixel(3)[3], 128);
        assert_eq!(pixel(4)[3], 0);

        let once = output;
        render(&mut renderer, &target);
        let mut twice = vec![0u8; once.len()];
        target.get_bytes(twice.as_mut_ptr().cast(), 16 * 4, region, 0);
        assert_eq!(
            twice, once,
            "reapplying the same tone must not flatten color again"
        );
    }

    #[test]
    fn backdrop_alpha_limit_should_reveal_window_backing_without_repeated_attenuation() {
        let device = metal::Device::system_default().expect("native Metal device required");
        #[cfg(not(feature = "runtime_shaders"))]
        let library = device
            .new_library_with_data(super::super::SHADERS_METALLIB)
            .unwrap();
        #[cfg(feature = "runtime_shaders")]
        let library = device
            .new_library_with_source(
                super::super::SHADERS_SOURCE_FILE,
                &metal::CompileOptions::new(),
            )
            .unwrap();
        let mut renderer = BackdropRenderer::new(&device, &library);
        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_width(32);
        descriptor.set_height(32);
        descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        descriptor.set_storage_mode(if device.has_unified_memory() {
            metal::MTLStorageMode::Shared
        } else {
            metal::MTLStorageMode::Managed
        });
        descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        let target = device.new_texture(&descriptor);
        let region = metal::MTLRegion::new_2d(0, 0, 32, 32);
        let mut original = [64u8, 32, 16, 255].repeat(32 * 32);
        original[(16 * 32 + 12) * 4..(16 * 32 + 13) * 4].copy_from_slice(&[16, 8, 4, 32]);
        original[(16 * 32 + 13) * 4..(16 * 32 + 14) * 4].fill(0);
        let bounds = Bounds::new(
            point(ScaledPixels(4.0), ScaledPixels(4.0)),
            size(ScaledPixels(24.0), ScaledPixels(24.0)),
        );
        let mut filter = BackdropFilter {
            bounds,
            content_mask: ContentMask {
                bounds: Bounds::new(bounds.origin, size(ScaledPixels(20.0), ScaledPixels(24.0))),
            },
            corner_radii: Corners::all(ScaledPixels(6.0)),
            opacity: 1.0,
            alpha_limit: 0.25,
            ..Default::default()
        };
        let queue = device.new_command_queue();
        let mut render = |input: &[u8], filter: &BackdropFilter| {
            target.replace_region(region, 0, input.as_ptr().cast(), 32 * 4);
            let commands = queue.new_command_buffer();
            renderer.encode(
                &device,
                commands,
                &target,
                filter,
                size(DevicePixels(32), DevicePixels(32)),
            );
            if !device.has_unified_memory() {
                let sync = commands.new_blit_command_encoder();
                sync.synchronize_resource(&target);
                sync.end_encoding();
            }
            commands.commit();
            commands.wait_until_completed();
            assert_eq!(commands.status(), metal::MTLCommandBufferStatus::Completed);
            let mut output = vec![0u8; input.len()];
            target.get_bytes(output.as_mut_ptr().cast(), 32 * 4, region, 0);
            output
        };
        let once = render(&original, &filter);
        let pixel = |bytes: &[u8], x: usize, y: usize| -> [u8; 4] {
            bytes[(y * 32 + x) * 4..(y * 32 + x + 1) * 4]
                .try_into()
                .unwrap()
        };
        assert_eq!(
            pixel(&once, 16, 16),
            [16, 8, 4, 64],
            "dense content must admit the native backing"
        );
        for (x, y) in [(12, 16), (13, 16), (0, 16), (4, 4), (26, 16)] {
            assert_eq!(
                pixel(&once, x, y),
                pixel(&original, x, y),
                "clear, already-thin, rounded and masked pixels must remain unchanged"
            );
        }
        let twice = render(&once, &filter);
        // Rounded edge antialiasing is partial coverage, like element opacity below. Fully
        // covered interior pixels must already be at the limit after the first treatment.
        for y in 10..22 {
            for x in 10..22 {
                assert_eq!(
                    pixel(&twice, x, y),
                    pixel(&once, x, y),
                    "nested filters must not repeatedly attenuate the same interior"
                );
            }
        }
        filter.opacity = 0.5;
        let partial = render(&original, &filter);
        assert_eq!(
            pixel(&partial, 16, 16),
            [40, 20, 10, 159],
            "element opacity must interpolate premultiplied RGBA"
        );
    }

    #[test]
    fn shadow_interior_exclusion_should_preserve_center_and_follow_rounded_shell() {
        let device = metal::Device::system_default().expect("native Metal device required");
        #[cfg(not(feature = "runtime_shaders"))]
        let library = device
            .new_library_with_data(super::super::SHADERS_METALLIB)
            .unwrap();
        #[cfg(feature = "runtime_shaders")]
        let library = device
            .new_library_with_source(
                super::super::SHADERS_SOURCE_FILE,
                &metal::CompileOptions::new(),
            )
            .unwrap();
        let pipeline = super::super::build_pipeline_state(
            &device,
            &library,
            "shadow interior exclusion test",
            "shadow_vertex",
            "shadow_fragment",
            metal::MTLPixelFormat::BGRA8Unorm,
        );
        let descriptor = metal::TextureDescriptor::new();
        descriptor.set_width(64);
        descriptor.set_height(64);
        descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        descriptor.set_storage_mode(if device.has_unified_memory() {
            metal::MTLStorageMode::Shared
        } else {
            metal::MTLStorageMode::Managed
        });
        descriptor.set_usage(metal::MTLTextureUsage::RenderTarget);
        let target = device.new_texture(&descriptor);
        let region = metal::MTLRegion::new_2d(0, 0, 64, 64);
        let original = vec![255u8; 64 * 64 * 4];
        let unit_vertices = [
            [0.0_f32, 0.0_f32],
            [1.0, 0.0],
            [0.0, 1.0],
            [0.0, 1.0],
            [1.0, 0.0],
            [1.0, 1.0],
        ];
        let unit_buffer = device.new_buffer_with_data(
            unit_vertices.as_ptr().cast(),
            std::mem::size_of_val(&unit_vertices) as u64,
            metal::MTLResourceOptions::StorageModeManaged,
        );
        let bounds = Bounds::new(
            point(ScaledPixels(20.0), ScaledPixels(20.0)),
            size(ScaledPixels(24.0), ScaledPixels(24.0)),
        );
        let viewport = size(DevicePixels(64), DevicePixels(64));
        let queue = device.new_command_queue();
        let render = |exclude_interior| {
            target.replace_region(region, 0, original.as_ptr().cast(), 64 * 4);
            let shadow = Shadow {
                order: 0,
                blur_radius: ScaledPixels(4.0),
                bounds,
                corner_radii: Corners::all(ScaledPixels(8.0)),
                content_mask: ContentMask {
                    bounds: Bounds::new(
                        point(ScaledPixels(0.0), ScaledPixels(0.0)),
                        size(ScaledPixels(64.0), ScaledPixels(64.0)),
                    ),
                },
                color: hsla(0.0, 0.0, 0.0, 1.0),
                exclude_bounds: bounds,
                exclude_corner_radii: Corners::all(ScaledPixels(8.0)),
                exclude_interior,
                pad: 0,
            };
            let shadow_buffer = device.new_buffer_with_data(
                (&shadow as *const Shadow).cast(),
                std::mem::size_of_val(&shadow) as u64,
                metal::MTLResourceOptions::StorageModeManaged,
            );
            let pass_descriptor = metal::RenderPassDescriptor::new();
            let color = pass_descriptor.color_attachments().object_at(0).unwrap();
            color.set_texture(Some(&target));
            color.set_load_action(metal::MTLLoadAction::Load);
            color.set_store_action(metal::MTLStoreAction::Store);
            let commands = queue.new_command_buffer();
            let encoder = commands.new_render_command_encoder(pass_descriptor);
            encoder.set_render_pipeline_state(&pipeline);
            encoder.set_viewport(metal::MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: 64.0,
                height: 64.0,
                znear: 0.0,
                zfar: 1.0,
            });
            encoder.set_vertex_buffer(
                super::super::ShadowInputIndex::Vertices as u64,
                Some(&unit_buffer),
                0,
            );
            encoder.set_vertex_buffer(
                super::super::ShadowInputIndex::Shadows as u64,
                Some(&shadow_buffer),
                0,
            );
            encoder.set_fragment_buffer(
                super::super::ShadowInputIndex::Shadows as u64,
                Some(&shadow_buffer),
                0,
            );
            encoder.set_vertex_bytes(
                super::super::ShadowInputIndex::ViewportSize as u64,
                std::mem::size_of_val(&viewport) as u64,
                (&viewport as *const Size<DevicePixels>).cast(),
            );
            encoder.draw_primitives_instanced(metal::MTLPrimitiveType::Triangle, 0, 6, 1);
            encoder.end_encoding();
            if !device.has_unified_memory() {
                let sync = commands.new_blit_command_encoder();
                sync.synchronize_resource(&target);
                sync.end_encoding();
            }
            commands.commit();
            commands.wait_until_completed();
            assert_eq!(commands.status(), metal::MTLCommandBufferStatus::Completed);
            let mut output = vec![0u8; original.len()];
            target.get_bytes(output.as_mut_ptr().cast(), 64 * 4, region, 0);
            output
        };

        let default_shadow = render(0);
        let outer_shadow = render(1);
        let channel = |pixels: &[u8], x: usize, y: usize| pixels[(y * 64 + x) * 4];
        assert!(
            channel(&default_shadow, 32, 32) < 250,
            "the default shadow must retain its filled center"
        );
        assert_eq!(
            channel(&outer_shadow, 32, 32),
            255,
            "the opt-in shadow must not repaint the shell center"
        );
        assert!(
            channel(&outer_shadow, 17, 32) < 250,
            "the opt-in shadow must preserve the exterior silhouette"
        );
        assert!(
            channel(&outer_shadow, 21, 21) < 250,
            "the cutout must follow rounded corners rather than a rectangular mask"
        );
    }
}

struct BackdropTextures {
    snapshot: metal::Texture,
    horizontal: metal::Texture,
    vertical: metal::Texture,
}

impl BackdropRenderer {
    pub(super) fn new(device: &metal::DeviceRef, library: &metal::LibraryRef) -> Self {
        let descriptor = metal::RenderPipelineDescriptor::new();
        descriptor.set_label("backdrop filter");
        let vertex = library
            .get_function("backdrop_vertex", None)
            .expect("backdrop vertex shader");
        let fragment = library
            .get_function("backdrop_fragment", None)
            .expect("backdrop fragment shader");
        descriptor.set_vertex_function(Some(&vertex));
        descriptor.set_fragment_function(Some(&fragment));
        let color = descriptor.color_attachments().object_at(0).unwrap();
        color.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        // Filtering replaces the captured premultiplied RGBA, including destination alpha.
        color.set_blending_enabled(false);
        Self {
            pipeline: device
                .new_render_pipeline_state(&descriptor)
                .expect("backdrop pipeline"),
            textures: None,
        }
    }

    pub(super) fn release(&mut self) {
        self.textures = None;
    }

    pub(super) fn encode(
        &mut self,
        device: &metal::DeviceRef,
        commands: &metal::CommandBufferRef,
        drawable: &metal::TextureRef,
        filter: &BackdropFilter,
        viewport: Size<DevicePixels>,
    ) {
        let width = viewport.width.0.max(1) as u64;
        let height = viewport.height.0.max(1) as u64;
        if self.textures.as_ref().is_none_or(|textures| {
            textures.snapshot.width() != width || textures.snapshot.height() != height
        }) {
            let make_texture = |width, height| {
                let descriptor = metal::TextureDescriptor::new();
                descriptor.set_width(width);
                descriptor.set_height(height);
                descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
                descriptor.set_storage_mode(metal::MTLStorageMode::Private);
                descriptor.set_usage(
                    metal::MTLTextureUsage::ShaderRead | metal::MTLTextureUsage::RenderTarget,
                );
                device.new_texture(&descriptor)
            };
            self.textures = Some(BackdropTextures {
                snapshot: make_texture(width, height),
                horizontal: make_texture(width.div_ceil(4), height.div_ceil(4)),
                vertical: make_texture(width.div_ceil(4), height.div_ceil(4)),
            });
        }
        let Some(textures) = self.textures.as_ref() else {
            return;
        };
        let copy = commands.new_blit_command_encoder();
        copy.copy_from_texture(
            drawable,
            0,
            0,
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
            metal::MTLSize {
                width,
                height,
                depth: 1,
            },
            &textures.snapshot,
            0,
            0,
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
        );
        copy.end_encoding();

        let first_pass = if filter.radius.0 > 0.0 { 0 } else { 2 };
        for pass_index in first_pass..=2 {
            let (target, source) = match pass_index {
                0 => (textures.horizontal.as_ref(), textures.snapshot.as_ref()),
                1 => (textures.vertical.as_ref(), textures.horizontal.as_ref()),
                _ if filter.radius.0 > 0.0 => (drawable, textures.vertical.as_ref()),
                _ => (drawable, textures.snapshot.as_ref()),
            };
            let pass = pass_index as f32;
            let target_width = target.width() as f32;
            let target_height = target.height() as f32;
            let sigma = if pass == 0.0 {
                filter.radius.0 * target_width / width as f32
            } else {
                filter.radius.0 * target_height / height as f32
            };
            let uniforms = [
                target_width,
                target_height,
                width as f32,
                height as f32,
                filter.bounds.origin.x.0,
                filter.bounds.origin.y.0,
                filter.bounds.size.width.0,
                filter.bounds.size.height.0,
                filter.content_mask.bounds.origin.x.0,
                filter.content_mask.bounds.origin.y.0,
                filter.content_mask.bounds.size.width.0,
                filter.content_mask.bounds.size.height.0,
                filter.corner_radii.top_left.0,
                filter.corner_radii.top_right.0,
                filter.corner_radii.bottom_right.0,
                filter.corner_radii.bottom_left.0,
                sigma,
                filter.opacity,
                pass,
                filter.alpha_limit,
                filter.tone.r,
                filter.tone.g,
                filter.tone.b,
                filter.tone.a,
            ];
            let descriptor = metal::RenderPassDescriptor::new();
            let color = descriptor.color_attachments().object_at(0).unwrap();
            color.set_texture(Some(target));
            color.set_load_action(metal::MTLLoadAction::Load);
            color.set_store_action(metal::MTLStoreAction::Store);
            let encoder = commands.new_render_command_encoder(descriptor);
            encoder.set_render_pipeline_state(&self.pipeline);
            encoder.set_viewport(metal::MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: target_width as f64,
                height: target_height as f64,
                znear: 0.0,
                zfar: 1.0,
            });
            let output = filter.bounds.intersect(&filter.content_mask.bounds);
            let halo = if pass < 2.0 {
                (3.0 * filter.radius.0).ceil() + 8.0
            } else {
                0.0
            };
            let scale_x = target_width / width as f32;
            let scale_y = target_height / height as f32;
            let x = ((output.origin.x.0 - halo) * scale_x)
                .floor()
                .max(0.0)
                .min(target_width) as u64;
            let y = ((output.origin.y.0 - halo) * scale_y)
                .floor()
                .max(0.0)
                .min(target_height) as u64;
            let right = ((output.origin.x.0 + output.size.width.0 + halo) * scale_x)
                .ceil()
                .max(x as f32)
                .min(target_width) as u64;
            let bottom = ((output.origin.y.0 + output.size.height.0 + halo) * scale_y)
                .ceil()
                .max(y as f32)
                .min(target_height) as u64;
            if right > x && bottom > y {
                encoder.set_scissor_rect(metal::MTLScissorRect {
                    x,
                    y,
                    width: right - x,
                    height: bottom - y,
                });
                encoder.set_fragment_bytes(
                    0,
                    std::mem::size_of_val(&uniforms) as u64,
                    uniforms.as_ptr().cast(),
                );
                encoder.set_fragment_texture(0, Some(source));
                encoder.set_fragment_texture(1, Some(&textures.snapshot));
                encoder.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
            }
            encoder.end_encoding();
        }
    }
}
