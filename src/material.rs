//! The terrain material: albedo + Terrarium heightmap + normal map, displaced
//! in the vertex shader. One material asset per tile (Bevy batches by
//! material); parameters are pushed from `RenderingConfig` by the plugin.

use crate::config::RenderingConfig;
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderRef, ShaderType, SpecializedMeshPipelineError,
};

pub const TERRAIN_SHADER_PATH: &str = "shaders/terrain.wgsl";

// allow: the ShaderType derive emits a hidden `check` fn whose spans land on
// the fields, tripping dead_code warnings on this bevy/encase version
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct TerrainParams {
    pub fog_color: Vec4,
    pub ambient: Vec4,
    pub sun_direction: Vec3,
    pub sun_scale: f32,
    pub fog_start: f32,
    pub fog_end: f32,
    pub height_scale: f32,
    pub normals_scale: f32,
    pub skirt_drop: f32,
}

impl TerrainParams {
    pub fn from_config(cfg: &RenderingConfig) -> Self {
        Self {
            fog_color: cfg.fog_color.to_linear().to_vec4(),
            ambient: cfg.ambient.to_linear().to_vec4(),
            sun_direction: cfg.sun_direction,
            sun_scale: cfg.sun_scale,
            fog_start: cfg.fog_start,
            fog_end: cfg.fog_end,
            height_scale: cfg.height_scale,
            normals_scale: cfg.normals_scale,
            skirt_drop: cfg.skirt_drop,
        }
    }
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct TerrainMaterial {
    #[texture(0)]
    #[sampler(1)]
    pub albedo: Handle<Image>,
    /// Terrarium heightmap — created as LINEAR (non-sRGB), or the decode is
    /// gamma-warped garbage.
    #[texture(2)]
    #[sampler(3)]
    pub heightmap: Handle<Image>,
    /// Normal map — linear as well.
    #[texture(4)]
    #[sampler(5)]
    pub normals: Handle<Image>,
    #[uniform(6)]
    pub params: TerrainParams,
}

impl Material for TerrainMaterial {
    fn vertex_shader() -> ShaderRef {
        TERRAIN_SHADER_PATH.into()
    }
    fn fragment_shader() -> ShaderRef {
        TERRAIN_SHADER_PATH.into()
    }
    fn specialize(
        _pipeline: &MaterialPipeline<Self>,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // we only consume position/normal/uv; pin the buffer layout explicitly
        let vertex_layout = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(2),
        ])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        Ok(())
    }
}
