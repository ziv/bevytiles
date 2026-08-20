//! The terrain material: albedo + Terrarium heightmap + normal map, displaced
//! in the vertex shader. One material asset per tile (Bevy batches by
//! material); parameters are pushed from `RenderingConfig` by the plugin.

// module-scoped: the ShaderType derive emits a phantom `check` fn that trips
// dead_code on this bevy/encase version; everything here is public anyway
#![allow(dead_code)]

use crate::config::RenderingConfig;
use bevy::asset::weak_handle;
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::mesh::MeshVertexBufferLayoutRef;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderRef, ShaderType, SpecializedMeshPipelineError,
};

/// Handle of the terrain WGSL (vertex displacement + lighting + fog). The
/// shader source lives in the crate's `assets/shaders/terrain.wgsl` but is
/// **embedded into the binary** at compile time and registered under this
/// handle by [`TerrainPlugin`](crate::TerrainPlugin) — consumers need no
/// asset-folder setup.
pub const TERRAIN_SHADER_HANDLE: Handle<Shader> =
    weak_handle!("b7c1a9e4-52d8-4f3a-9c06-8e51d27f4b19");

// allow: the ShaderType derive emits a hidden `check` fn whose spans land on
// the fields, tripping dead_code warnings on this bevy/encase version
/// The uniform block handed to `terrain.wgsl` — the GPU mirror of
/// [`RenderingConfig`]. Field order must match the WGSL `TerrainParams`
/// struct declaration exactly (encase lays it out in order).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, ShaderType)]
pub struct TerrainParams {
    /// Fog color, linear RGBA.
    pub fog_color: Vec4,
    /// Ambient light color, linear RGBA.
    pub ambient: Vec4,
    /// Sun direction (normalized in the shader).
    pub sun_direction: Vec3,
    /// Sun light intensity.
    pub sun_scale: f32,
    /// Fog start distance (meters from the camera).
    pub fog_start: f32,
    /// Fog end distance (meters from the camera).
    pub fog_end: f32,
    /// Terrain relief exaggeration applied to the decoded heights.
    pub height_scale: f32,
    /// Normal-map contrast multiplier.
    pub normals_scale: f32,
    /// Vertical skirt drop (meters) applied to mesh-edge vertices.
    pub skirt_drop: f32,
}

impl TerrainParams {
    /// Convert the user-facing config (sRGB [`Color`]s, plain fields) into
    /// the linear-space uniform block.
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

/// One material per resident tile: the three tile textures plus the shared
/// parameter block. Bevy uploads/binds these; dropping the handles (with the
/// tile entity) frees the GPU resources.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct TerrainMaterial {
    /// Satellite imagery (sRGB).
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
    /// Shader parameters; kept in sync with [`RenderingConfig`] by the
    /// plugin's `sync_rendering` system.
    #[uniform(6)]
    pub params: TerrainParams,
}

impl Material for TerrainMaterial {
    fn vertex_shader() -> ShaderRef {
        TERRAIN_SHADER_HANDLE.into()
    }
    fn fragment_shader() -> ShaderRef {
        TERRAIN_SHADER_HANDLE.into()
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
