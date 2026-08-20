//! The raytiles demo, on Bevy: fly over the Negev with a loading screen,
//! large-world rebasing, ground-collision crash detection, and sky-matched
//! fog. Controls: W/S pitch, A/D yaw, Up/Down throttle, R reset after crash.

use bevy::prelude::*;
use bevytiles::prelude::*;

// the Negev (same anchor as the raytiles demo)
const LAT: f64 = 35.97391;
const LON: f64 = -113.76892;

const SKY: Color = Color::srgb_u8(102, 191, 255); // raylib SKYBLUE
const REBASE_THRESHOLD: f32 = 4096.0;

fn main() {
    let mut rendering = RenderingConfig::default();
    rendering.fog_color = SKY;
    rendering.ambient = Color::srgb_u8(200, 200, 200);

    let mut network = NetworkConfig::default();
    network.threads = 8;

    let mut world = WorldConfig::from_lat_lon(LAT, LON);
    world.skirt_overlap = [1.01; ZOOM_LEVELS];
    // opt into greater zoom: imagery fetches natively, heightmaps above z15
    // are synthesized, normals default to flat
    world.max_zoom = 17;

    App::new()
        .add_plugins(DefaultPlugins)
        .insert_resource(ClearColor(SKY))
        .insert_resource(world)
        .insert_resource(rendering)
        .insert_resource(network)
        .add_plugins(TerrainPlugin)
        .init_resource::<Flight>()
        .add_systems(Startup, setup)
        .add_systems(Update, (fly, rebase_large_world, crash_check, loading_ui, hud).chain())
        .run();
}

#[derive(Resource)]
struct Flight {
    speed: f32,
    crashed: bool,
}

impl Default for Flight {
    fn default() -> Self {
        Self { speed: 120.0, crashed: false }
    }
}

#[derive(Component)]
struct LoadingText;

#[derive(Component)]
struct HudText;

#[derive(Component)]
struct CrashText;

fn setup(mut commands: Commands, world: Res<WorldConfig>) {
    commands.spawn((
        Camera3d::default(),
        TerrainCamera,
        Projection::Perspective(PerspectiveProjection {
            fov: 60f32.to_radians(),
            near: 1.0,
            far: 400_000.0,
            ..Default::default()
        }),
        Transform::from_translation(world.initial_position(5_000.0))
            .looking_at(world.initial_position(5_000.0) + Vec3::new(-1000.0, -300.0, -1000.0), Vec3::Y),
    ));

    commands.spawn((
        LoadingText,
        Text::new("Loading... 0%"),
        TextFont { font_size: 42.0, ..Default::default() },
        TextColor(Color::WHITE),
        Node { position_type: PositionType::Absolute, left: Val::Percent(38.0), top: Val::Percent(45.0), ..Default::default() },
    ));
    commands.spawn((
        HudText,
        Text::new(""),
        TextFont { font_size: 16.0, ..Default::default() },
        TextColor(Color::WHITE),
        Node { position_type: PositionType::Absolute, left: Val::Px(10.0), top: Val::Px(10.0), ..Default::default() },
    ));
}

/// Simple airplane-style fly camera: always moving forward.
fn fly(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut flight: ResMut<Flight>,
    mut cams: Query<&mut Transform, With<TerrainCamera>>,
) {
    if flight.crashed {
        return;
    }
    let Ok(mut tf) = cams.single_mut() else { return };
    let dt = time.delta_secs();

    let mut yaw = 0.0f32;
    let mut pitch = 0.0f32;
    if keys.pressed(KeyCode::KeyA) {
        yaw += 0.8 * dt;
    }
    if keys.pressed(KeyCode::KeyD) {
        yaw -= 0.8 * dt;
    }
    if keys.pressed(KeyCode::KeyW) {
        pitch -= 0.6 * dt;
    }
    if keys.pressed(KeyCode::KeyS) {
        pitch += 0.6 * dt;
    }
    if keys.pressed(KeyCode::ArrowUp) {
        flight.speed = (flight.speed * (1.0 + dt)).min(3_000.0);
    }
    if keys.pressed(KeyCode::ArrowDown) {
        flight.speed = (flight.speed * (1.0 - dt)).max(20.0);
    }

    tf.rotate_y(yaw);
    let right = tf.right();
    tf.rotate_axis(right, pitch);
    let forward = tf.forward();
    tf.translation += forward * flight.speed * dt;
}

/// Keep the user-space camera near the origin: when it drifts past the
/// threshold, shift the camera AND the world offset by the same amount —
/// preserving `absolute = user − offset`. The plugin rebakes tile transforms
/// when TerrainAnchor changes.
fn rebase_large_world(mut anchor: ResMut<TerrainAnchor>, mut cams: Query<&mut Transform, With<TerrainCamera>>) {
    let Ok(mut tf) = cams.single_mut() else { return };
    let mut shift = Vec3::ZERO;
    if tf.translation.x.abs() > REBASE_THRESHOLD {
        shift.x = -REBASE_THRESHOLD.copysign(tf.translation.x);
    }
    if tf.translation.z.abs() > REBASE_THRESHOLD {
        shift.z = -REBASE_THRESHOLD.copysign(tf.translation.z);
    }
    if shift != Vec3::ZERO {
        tf.translation += shift;
        anchor.world_offset += shift;
    }
}

fn crash_check(
    mut commands: Commands,
    mut flight: ResMut<Flight>,
    grids: Res<HeightGrids>,
    world: Res<WorldConfig>,
    anchor: Res<TerrainAnchor>,
    keys: Res<ButtonInput<KeyCode>>,
    mut cams: Query<&mut Transform, With<TerrainCamera>>,
    crash_text: Query<Entity, With<CrashText>>,
) {
    let Ok(mut tf) = cams.single_mut() else { return };

    if flight.crashed {
        if keys.just_pressed(KeyCode::KeyR) {
            flight.crashed = false;
            tf.translation = world.initial_position(5_000.0) + anchor.world_offset;
            for e in &crash_text {
                commands.entity(e).despawn();
            }
        }
        return;
    }

    let ground = ground_height(&grids, &world, &anchor, tf.translation).unwrap_or(0.0);
    if ground > tf.translation.y {
        flight.crashed = true;
        commands.spawn((
            CrashText,
            Text::new("You crashed! Press R to reset."),
            TextFont { font_size: 36.0, ..Default::default() },
            TextColor(Color::WHITE),
            Node { position_type: PositionType::Absolute, left: Val::Percent(30.0), top: Val::Percent(40.0), ..Default::default() },
        ));
    }
}

fn loading_ui(
    mut commands: Commands,
    status: Res<TerrainStatus>,
    mut texts: Query<(Entity, &mut Text), With<LoadingText>>,
) {
    for (entity, mut text) in &mut texts {
        if status.loading {
            text.0 = format!("Loading... {:.1}%", status.progress * 100.0);
        } else {
            commands.entity(entity).despawn();
        }
    }
}

fn hud(
    status: Res<TerrainStatus>,
    flight: Res<Flight>,
    anchor: Res<TerrainAnchor>,
    cams: Query<&Transform, With<TerrainCamera>>,
    mut texts: Query<&mut Text, With<HudText>>,
) {
    let Ok(cam) = cams.single() else { return };
    for mut text in &mut texts {
        text.0 = format!(
            "W/S pitch  A/D yaw  Up/Down throttle ({:.0} m/s)\nuser P {:.0} {:.0} {:.0}   offset {:.0} {:.0}\ntiles resident: {}",
            flight.speed,
            cam.translation.x,
            cam.translation.y,
            cam.translation.z,
            anchor.world_offset.x,
            anchor.world_offset.z,
            status.resident,
        );
    }
}
