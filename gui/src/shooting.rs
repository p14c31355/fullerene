use bevy::prelude::*;
use nalgebra::Vector2;

struct Player;
struct Bullet;

fn main() {
    App::build()
        .add_startup_system(setup.system())
        .add_system(player_movement.system())
        .add_system(spawn_bullet.system())
        .add_system(bullet_movement.system())
        .add_plugins(DefaultPlugins)
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>, mut materials: ResMut<Assets<ColorMaterial>>) {
    commands.spawn_bundle(OrthographicCameraBundle::new_2d());

    commands.spawn_bundle(SpriteBundle {
        material: materials.add(Color::rgb(0.0, 1.0, 0.0).into()),
        sprite: Sprite::new(Vec2::new(30.0, 30.0)),
        ..Default::default()
    })
    .insert(Player);
}

fn player_movement(input: Res<Input<KeyCode>>, mut query: Query<&mut Transform, With<Player>>) {
    for mut transform in query.iter_mut() {
        let speed = 500.0;

        if input.pressed(KeyCode::Left) {
            transform.translation.x -= speed * Timer::time_since_startup().as_secs_f32().cos();
        }

        if input.pressed(KeyCode::Right) {
            transform.translation.x += speed * Timer::time_since_startup().as_secs_f32().cos();
        }

        if input.pressed(KeyCode::Up) {
            transform.translation.y += speed * Timer::time_since_startup().as_secs_f32().sin();
        }

        if input.pressed(KeyCode::Down) {
            transform.translation.y -= speed * Timer::time_since_startup().as_secs_f32().sin();
        }
    }
}

fn spawn_bullet(
    input: Res<Input<KeyCode>>,
    time: Res<Time>,
    mut commands: Commands,
    player_query: Query<&Transform, With<Player>>,
) {
    if input.just_pressed(KeyCode::Space) {
        for player_transform in player_query.iter() {
            commands.spawn_bundle(SpriteBundle {
                material: ColorMaterial {
                    color: Color::rgb(1.0, 0.0, 0.0),
                    texture: None,
                },
                sprite: Sprite::new(Vec2::new(10.0, 10.0)),
                transform: Transform::from_translation(player_transform.translation),
                ..Default::default()
            })
            .insert(Bullet);
        }
    }
}

fn bullet_movement(time: Res<Time>, mut query: Query<(&Bullet, &mut Transform)>) {
    let speed = 800.0;

    for (_, mut transform) in query.iter_mut() {
        transform.translation.y += speed * time.delta_seconds();
    }
}
