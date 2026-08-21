//! The raytiles demo, on Bevy: fly over real-world terrain with a loading
//! screen, large-world rebasing, ground-collision crash detection, and
//! sky-matched fog.
//!
//! Controls: **A/D** roll, **Q/E** yaw, **W/S** pitch, **+/-** throttle,
//! **R** reset after a crash. Change [`LAT`]/[`LON`] to fly anywhere.
//!
//! Runs natively (`cargo run --example demo`) and in the browser — see
//! `web/README.md`.

use bevy::prelude::*;
use bevytiles::prelude::*;

/// World anchor: the Grand Canyon. (The raytiles demo also ships anchors for
/// the Negev, the Dolomites, and London — any lat/lon works.)
const LAT: f64 = 35.97391;
/// See [`LAT`].
const LON: f64 = -113.76892;

/// Sky/fog color (raylib's SKYBLUE, for parity with the C++ demo).
const SKY: Color = Color::srgb_u8(102, 191, 255);
/// How quickly the controls ease toward their target rate (1/s): higher is
/// snappier, lower is floatier.
const CONTROL_RESPONSE: f32 = 5.0;
/// User-space drift (meters) that triggers a large-world rebase.
const REBASE_THRESHOLD: f32 = 4096.0;

fn main() {
    let rendering = RenderingConfig {
        fog_color: SKY,
        skirt_drop: 1000.0,
        ambient: Color::srgb_u8(200, 200, 200),
        ..Default::default()
    };

    let network = NetworkConfig {
        threads: 8,
        ..Default::default()
    };

    let mut world = WorldConfig::from_lat_lon(LAT, LON);
    world.skirt_overlap = [1.01; ZOOM_LEVELS];
    // opt into greater zoom: imagery fetches natively, heightmaps above z15
    // are synthesized, normals default to flat
    world.max_zoom = 17;

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "bevytiles demo".into(),
                // on the web, attach to the canvas in web/index.html and
                // keep browser shortcuts (F5, Ctrl+R, ...) working
                #[cfg(target_arch = "wasm32")]
                canvas: Some("#bevytiles".into()),
                #[cfg(target_arch = "wasm32")]
                fit_canvas_to_parent: true,
                #[cfg(target_arch = "wasm32")]
                prevent_default_event_handling: false,
                ..default()
            }),
            ..default()
        }))
        .insert_resource(ClearColor(SKY))
        .insert_resource(world)
        .insert_resource(rendering)
        .insert_resource(network)
        .add_plugins(TerrainPlugin)
        .init_resource::<Flight>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (fly, rebase_large_world, crash_check, loading_ui, hud).chain(),
        )
        .run();
}

/// Demo flight state: forward speed, smoothed angular velocity, and whether
/// we hit the ground.
#[derive(Resource)]
struct Flight {
    speed: f32,
    /// Current angular velocity (rad/s): x = pitch, y = yaw, z = roll.
    /// Eased toward the key-derived target each frame for smooth control.
    ang_vel: Vec3,
    crashed: bool,
}

impl Default for Flight {
    fn default() -> Self {
        Self {
            speed: 120.0,
            ang_vel: Vec3::ZERO,
            crashed: false,
        }
    }
}

#[derive(Component)]
struct LoadingText;

#[derive(Component)]
struct HudText;

#[derive(Component)]
struct CrashText;

/// Spawn the streaming camera (marked [`TerrainCamera`]) and the UI texts.
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
        Transform::from_translation(world.initial_position(5_000.0)).looking_at(
            world.initial_position(5_000.0) + Vec3::new(-1000.0, -300.0, -1000.0),
            Vec3::Y,
        ),
    ));

    commands.spawn((
        LoadingText,
        Text::new("Loading... 0%"),
        TextFont {
            font_size: FontSize::Px(42.0),
            ..Default::default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Percent(38.0),
            top: Val::Percent(45.0),
            ..Default::default()
        },
    ));
    commands.spawn((
        HudText,
        Text::new(""),
        TextFont {
            font_size: FontSize::Px(16.0),
            ..Default::default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(10.0),
            top: Val::Px(10.0),
            ..Default::default()
        },
    ));
}

/// Airplane-style fly camera, always moving forward. All rotations are
/// around the camera's LOCAL axes, so yawing while banked turns like an
/// aircraft: A/D roll, Q/E yaw, W/S pitch, +/- throttle.
///
/// Smoothing: keys set a TARGET angular velocity; the actual velocity eases
/// toward it exponentially ([`CONTROL_RESPONSE`], frame-rate independent),
/// so inputs ramp in and glide out instead of snapping.
fn fly(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut flight: ResMut<Flight>,
    mut cams: Query<&mut Transform, With<TerrainCamera>>,
) {
    if flight.crashed {
        return;
    }
    let Ok(mut tf) = cams.single_mut() else {
        return;
    };
    let dt = time.delta_secs();

    // target angular velocity from the keys (rad/s)
    let mut target = Vec3::ZERO;
    if keys.pressed(KeyCode::KeyA) {
        target.z += 1.2; // bank left
    }
    if keys.pressed(KeyCode::KeyD) {
        target.z -= 1.2; // bank right
    }
    if keys.pressed(KeyCode::KeyQ) {
        target.y += 0.8; // nose left
    }
    if keys.pressed(KeyCode::KeyE) {
        target.y -= 0.8; // nose right
    }
    if keys.pressed(KeyCode::KeyW) {
        target.x -= 0.6; // nose down
    }
    if keys.pressed(KeyCode::KeyS) {
        target.x += 0.6; // nose up
    }
    if keys.pressed(KeyCode::Equal) || keys.pressed(KeyCode::NumpadAdd) {
        flight.speed = (flight.speed * (1.0 + dt)).min(3_000.0);
    }
    if keys.pressed(KeyCode::Minus) || keys.pressed(KeyCode::NumpadSubtract) {
        flight.speed = (flight.speed * (1.0 - dt)).max(20.0);
    }

    // ease the actual rate toward the target (exponential, dt-independent)
    let blend = 1.0 - (-CONTROL_RESPONSE * dt).exp();
    flight.ang_vel = flight.ang_vel.lerp(target, blend);

    tf.rotate_local_z(flight.ang_vel.z * dt);
    tf.rotate_local_y(flight.ang_vel.y * dt);
    tf.rotate_local_x(flight.ang_vel.x * dt);
    let forward = tf.forward();
    tf.translation += forward * flight.speed * dt;
}

/// Keep the user-space camera near the origin: when it drifts past the
/// threshold, shift the camera AND the world offset by the same amount —
/// preserving `absolute = user − offset`. The plugin rebakes tile transforms
/// when TerrainAnchor changes.
fn rebase_large_world(
    mut anchor: ResMut<TerrainAnchor>,
    mut cams: Query<&mut Transform, With<TerrainCamera>>,
) {
    let Ok(mut tf) = cams.single_mut() else {
        return;
    };
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

/// Compare the camera altitude against [`ground_height`]; below ground =
/// crash. `R` respawns at the initial position (offset-corrected).
#[allow(clippy::too_many_arguments)] // bevy system: each param is an injected resource
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
    let Ok(mut tf) = cams.single_mut() else {
        return;
    };

    if flight.crashed {
        if keys.just_pressed(KeyCode::KeyR) {
            flight.crashed = false;
            flight.ang_vel = Vec3::ZERO;
            // reset orientation too — with roll you can crash inverted
            *tf =
                Transform::from_translation(world.initial_position(5_000.0) + anchor.world_offset)
                    .looking_at(
                        world.initial_position(5_000.0)
                            + anchor.world_offset
                            + Vec3::new(-1000.0, -300.0, -1000.0),
                        Vec3::Y,
                    );
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
            TextFont {
                font_size: FontSize::Px(36.0),
                ..Default::default()
            },
            TextColor(Color::WHITE),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(30.0),
                top: Val::Percent(40.0),
                ..Default::default()
            },
        ));
    }
}

/// Splash text while [`TerrainStatus::loading`]; despawns itself after.
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

/// Corner HUD: controls, speed, positions, resident-tile count.
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
            "A/D roll  Q/E yaw  W/S pitch  +/- throttle ({:.0} m/s)\nuser P {:.0} {:.0} {:.0}   offset {:.0} {:.0}\ntiles resident: {}",
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
