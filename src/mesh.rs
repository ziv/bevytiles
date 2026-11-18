//! The shared per-zoom tile mesh: a flat grid the vertex shader displaces by
//! the heightmap, plus a skirt — a ring of vertices under the grid's edge,
//! joined to it by vertical quads — that the shader lowers by
//! [`RenderingConfig::skirt_drop`](crate::config::RenderingConfig). The skirt
//! hangs below the surface and fills any crack between neighbouring tiles,
//! while the surface itself, edges included, stays where the heightmap puts it.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, MeshVertexAttribute, PrimitiveTopology, VertexFormat};
use bevy::prelude::*;

/// 1.0 on the skirt's lowered ring, 0.0 on the surface.
pub const ATTRIBUTE_SKIRT: MeshVertexAttribute =
    MeshVertexAttribute::new("TerrainSkirt", 0x7e1e5c17, VertexFormat::Float32);

/// Quads a side of the mesh of a tile at `zoom`: 4 at the base zoom,
/// doubling per zoom up to 256 (a heightmap's texels).
pub fn mesh_res(world: &crate::config::WorldConfig, zoom: u8) -> u32 {
    (4u32 << (zoom - world.base_zoom).min(6)).min(256)
}

/// A square tile mesh of side `size` with `res` quads a side, centred on the
/// origin in XZ. UVs run `[0, 1]` along +X and +Z, as Bevy's plane, so the
/// shader samples the heightmap by UV; each skirt vertex repeats the UV of the
/// edge vertex above it.
pub fn tile_mesh(size: f32, res: u32) -> Mesh {
    let n = res + 1;
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut skirt = Vec::new();
    let mut indices = Vec::new();

    for z in 0..n {
        for x in 0..n {
            let (u, v) = (x as f32 / res as f32, z as f32 / res as f32);
            positions.push([(u - 0.5) * size, 0.0, (v - 0.5) * size]);
            uvs.push([u, v]);
            skirt.push(0.0);
        }
    }
    let at = |x: u32, z: u32| z * n + x;
    for z in 0..res {
        for x in 0..res {
            let quad = at(x, z);
            indices.extend([quad + n + 1, quad + 1, quad + n, quad, quad + n, quad + 1]);
        }
    }

    // the edge walked counter-clockwise seen from above (+Y), so each skirt
    // quad, wound like the surface, faces outward
    let mut ring = Vec::new();
    ring.extend((0..res).map(|x| at(x, n - 1)));
    ring.extend((0..res).map(|z| at(n - 1, n - 1 - z)));
    ring.extend((0..res).map(|x| at(n - 1 - x, 0)));
    ring.extend((0..res).map(|z| at(0, z)));
    let first = positions.len() as u32;
    for &top in &ring {
        positions.push(positions[top as usize]);
        uvs.push(uvs[top as usize]);
        skirt.push(1.0);
    }
    let count = ring.len() as u32;
    for i in 0..count {
        let j = (i + 1) % count;
        let (top_a, top_b) = (ring[i as usize], ring[j as usize]);
        let (low_a, low_b) = (first + i, first + j);
        indices.extend([top_a, low_a, top_b, top_b, low_a, low_b]);
    }

    let normals = vec![[0.0, 1.0, 0.0]; positions.len()];
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_indices(Indices::U32(indices))
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_attribute(ATTRIBUTE_SKIRT, skirt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skirt_repeats_every_edge_vertex_and_nothing_else() {
        let res = 4;
        let mesh = tile_mesh(100.0, res);
        let Some(bevy::mesh::VertexAttributeValues::Float32(skirt)) =
            mesh.attribute(ATTRIBUTE_SKIRT)
        else {
            panic!("no skirt attribute");
        };
        let surface = ((res + 1) * (res + 1)) as usize;
        assert_eq!(skirt.len(), surface + 4 * res as usize);
        assert!(skirt[..surface].iter().all(|&s| s == 0.0));
        assert!(skirt[surface..].iter().all(|&s| s == 1.0));
        let Some(bevy::mesh::VertexAttributeValues::Float32x2(uvs)) =
            mesh.attribute(Mesh::ATTRIBUTE_UV_0)
        else {
            panic!("no uvs");
        };
        for uv in &uvs[surface..] {
            let on_edge = uv.iter().any(|&c| c == 0.0 || c == 1.0);
            assert!(on_edge, "skirt vertex off the edge at {uv:?}");
        }
    }

    #[test]
    fn the_skirt_quads_face_outward() {
        let mesh = tile_mesh(100.0, 2);
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("no positions");
        };
        let Some(Indices::U32(indices)) = mesh.indices() else {
            panic!("no indices");
        };
        let surface_triangles = 2 * 2 * 2;
        for tri in indices.chunks(3).skip(surface_triangles) {
            let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| {
                let mut v = Vec3::from(p[i as usize]);
                if i as usize >= 9 {
                    v.y -= 1.0;
                }
                v
            });
            let normal = (b - a).cross(c - a);
            let centre = (a + b + c) / 3.0;
            assert!(
                normal.dot(Vec3::new(centre.x, 0.0, centre.z)) > 0.0,
                "an inward skirt triangle {tri:?}"
            );
        }
    }
}
