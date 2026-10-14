use wgpu::{
    BlendState, ColorTargetState, ColorWrites, FragmentState, MultisampleState,
    PipelineCompilationOptions, PipelineLayoutDescriptor, PrimitiveState, RenderPass,
    RenderPipeline, RenderPipelineDescriptor, ShaderModuleDescriptor, ShaderSource, TextureFormat,
    VertexState,
};

const SHADER: &str = r"
@vertex
fn full_screen_triangle(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(positions[index], 0.0, 1.0);
}

@fragment
fn selection() -> @location(0) vec4<f32> {
    return vec4<f32>(0.20, 0.42, 0.70, 0.48);
}

@fragment
fn caret() -> @location(0) vec4<f32> {
    return vec4<f32>(0.88, 0.90, 0.94, 1.0);
}
";

#[derive(Clone, Copy, Debug)]
pub struct OverlayRect {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

pub struct OverlayRenderer {
    selection: RenderPipeline,
    caret: RenderPipeline,
}

impl OverlayRenderer {
    #[must_use]
    pub fn new(device: &wgpu::Device, format: TextureFormat) -> Self {
        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("Vanta overlay shader"),
            source: ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("Vanta overlay layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let create = |label, entry_point| {
            device.create_render_pipeline(&RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: VertexState {
                    module: &shader,
                    entry_point: Some("full_screen_triangle"),
                    buffers: &[],
                    compilation_options: PipelineCompilationOptions::default(),
                },
                fragment: Some(FragmentState {
                    module: &shader,
                    entry_point: Some(entry_point),
                    targets: &[Some(ColorTargetState {
                        format,
                        blend: Some(BlendState::ALPHA_BLENDING),
                        write_mask: ColorWrites::ALL,
                    })],
                    compilation_options: PipelineCompilationOptions::default(),
                }),
                primitive: PrimitiveState::default(),
                depth_stencil: None,
                multisample: MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            selection: create("Vanta selection pipeline", "selection"),
            caret: create("Vanta caret pipeline", "caret"),
        }
    }

    pub fn draw_selection<'pass>(
        &'pass self,
        pass: &mut RenderPass<'pass>,
        rectangles: &[OverlayRect],
        surface_width: u32,
        surface_height: u32,
    ) {
        Self::draw(
            pass,
            &self.selection,
            rectangles,
            surface_width,
            surface_height,
        );
    }

    pub fn draw_caret<'pass>(
        &'pass self,
        pass: &mut RenderPass<'pass>,
        rectangle: OverlayRect,
        surface_width: u32,
        surface_height: u32,
    ) {
        Self::draw(
            pass,
            &self.caret,
            std::slice::from_ref(&rectangle),
            surface_width,
            surface_height,
        );
    }

    fn draw<'pass>(
        pass: &mut RenderPass<'pass>,
        pipeline: &'pass RenderPipeline,
        rectangles: &[OverlayRect],
        surface_width: u32,
        surface_height: u32,
    ) {
        pass.set_pipeline(pipeline);
        for rectangle in rectangles {
            if let Some((x, y, width, height)) = rectangle.scissor(surface_width, surface_height) {
                pass.set_scissor_rect(x, y, width, height);
                pass.draw(0..3, 0..1);
            }
        }
        pass.set_scissor_rect(0, 0, surface_width, surface_height);
    }
}

impl OverlayRect {
    // UI coordinates have already been bounded by the physical surface. Pixel
    // precision beyond a u32 framebuffer is neither observable nor supported.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    fn scissor(self, surface_width: u32, surface_height: u32) -> Option<(u32, u32, u32, u32)> {
        let left = self.left.max(0.0).min(surface_width as f32).floor() as u32;
        let top = self.top.max(0.0).min(surface_height as f32).floor() as u32;
        let right = (self.left + self.width)
            .max(0.0)
            .min(surface_width as f32)
            .ceil() as u32;
        let bottom = (self.top + self.height)
            .max(0.0)
            .min(surface_height as f32)
            .ceil() as u32;
        (right > left && bottom > top).then_some((left, top, right - left, bottom - top))
    }
}
