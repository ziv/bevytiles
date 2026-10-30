// Terrain displacement + lighting + fog. Port of the raytiles GLSL pair.

import bevy_pbr::render::{
    mesh_functions,
    view_transformations::position_world_to_clip,
    mesh_view_bindings::view,
};

struct TerrainParams {
    fog_color: vec4<f32>,
    ambient: vec4<f32>,
    sun_direction: vec3<f32>,
    sun_scale: f32,
    fog_start: f32,
    fog_end: f32,
    height_scale: f32,
    normals_scale: f32,
    skirt_drop: f32,
}

// the material bind group index is a constant Bevy supplies (WESL since bevy
// 0.20; a `#{MATERIAL_BIND_GROUP}` shader def before)
@group(constants::MATERIAL_BIND_GROUP) @binding(0) var albedo_tex: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var albedo_smp: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(2) var height_tex: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(3) var height_smp: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(4) var normal_tex: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(5) var normal_smp: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(6) var<uniform> params: TerrainParams;
@group(constants::MATERIAL_BIND_GROUP) @binding(7) var edge_tex: texture_2d<f32>;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) skirt: f32,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world_position: vec3<f32>,
};

// Terrarium decode: h = r*256 + g + b/256 - 32768 (channels are 0..1 here).
// Must stay in lockstep with the CPU decoders (synth.rs, height.rs).
fn terrarium_height(uv: vec2<f32>) -> f32 {
    let c = textureSampleLevel(height_tex, height_smp, uv, 0.0).rgb * 255.0;
    return c.r * 256.0 + c.g + c.b / 256.0 - 32768.0;
}

// A vertex's height (meters): on the tile's edge the one it shares with its
// neighbours (edges.rs, a row per side: v = 0, v = 1, u = 0, u = 1), else
// the heightmap's.
fn vertex_height(uv: vec2<f32>) -> f32 {
    let res = f32(textureDimensions(edge_tex).x - 1u);
    let along_u = i32(round(uv.x * res));
    let along_v = i32(round(uv.y * res));
    if uv.y <= 0.0 {
        return textureLoad(edge_tex, vec2<i32>(along_u, 0), 0).r;
    }
    if uv.y >= 1.0 {
        return textureLoad(edge_tex, vec2<i32>(along_u, 1), 0).r;
    }
    if uv.x <= 0.0 {
        return textureLoad(edge_tex, vec2<i32>(along_v, 2), 0).r;
    }
    if uv.x >= 1.0 {
        return textureLoad(edge_tex, vec2<i32>(along_v, 3), 0).r;
    }
    return terrarium_height(uv);
}

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    var out: VertexOutput;

    var pos = v.position;
    pos.y += vertex_height(v.uv) * params.height_scale;

    // the skirt's ring hangs below the edge above it, filling LOD cracks;
    // the surface's own edge vertices stay put
    pos.y -= params.skirt_drop * params.height_scale * v.skirt;

    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let world_position = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(pos, 1.0));
    out.clip_position = position_world_to_clip(world_position.xyz);
    out.uv = v.uv;
    out.world_position = world_position.xyz;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex = textureSample(albedo_tex, albedo_smp, in.uv);

    var n = textureSample(normal_tex, normal_smp, in.uv).rgb * 2.0 - 1.0;
    n = vec3(n.xy * params.normals_scale, n.z);
    n = normalize(n);

    let sun = max(dot(n, normalize(params.sun_direction)), 0.0) * params.sun_scale;
    let lighting = clamp(params.ambient + vec4(sun, sun, sun, sun), vec4(0.0), vec4(1.0));
    let lit = tex * lighting;

    let dist = distance(in.world_position, view.world_position);
    let fog = clamp((dist - params.fog_start) / (params.fog_end - params.fog_start), 0.0, 1.0);
    return mix(lit, params.fog_color, fog);
}
