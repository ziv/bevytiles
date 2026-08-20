// Terrain displacement + lighting + fog. Port of the raytiles GLSL pair.

#import bevy_pbr::{
    mesh_functions,
    view_transformations::position_world_to_clip,
    mesh_view_bindings::view,
}

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

@group(2) @binding(0) var albedo_tex: texture_2d<f32>;
@group(2) @binding(1) var albedo_smp: sampler;
@group(2) @binding(2) var height_tex: texture_2d<f32>;
@group(2) @binding(3) var height_smp: sampler;
@group(2) @binding(4) var normal_tex: texture_2d<f32>;
@group(2) @binding(5) var normal_smp: sampler;
@group(2) @binding(6) var<uniform> params: TerrainParams;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
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

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    var out: VertexOutput;

    var pos = v.position;
    pos.y += terrarium_height(v.uv) * params.height_scale;

    // edge vertices drop by the skirt amount to hide LOD cracks
    let e = 0.000001;
    let edge = clamp(
        step(v.uv.x, e) + step(1.0 - e, v.uv.x) + step(v.uv.y, e) + step(1.0 - e, v.uv.y),
        0.0, 1.0,
    );
    pos.y -= params.skirt_drop * params.height_scale * edge;

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
