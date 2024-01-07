use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, Serialize)]
struct AssetConfig {
    path: String,
    color: [f32; 3],
}

struct Cube;

fn main() {
    App::build()
    // JSONファイルからアセットの設定を読み込む
    let asset_config: Vec<AssetConfig> = load_asset_config("assets.json");
        .add_startup_system(setup.system()) //各異名関数を main に投入している
        .add_system(cube_movement.system())
        .add_system(cube_rotation.system())
        .add_system(input_system.system())
        .add_startup_system(move |world| setup.system().label("setup"))
        .add_startup_stage_after(stage::SETUP, "load_assets", SystemStage::single(load_assets.system()))
        .add_startup_system_to_stage("load_assets", move |world| setup_assets.system().label("setup_assets"))
        .add_startup_stage_after("load_assets", "spawn_cube", SystemStage::single(spawn_cube.system()))
        .add_plugins(DefaultPlugins)
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>, mut materials: ResMut<Assets<ColorMaterial>>) {
    // カメラの追加
    commands.spawn_bundle(OrthographicCameraBundle::new_2d());

    // 立方体の追加
    commands.spawn_bundle(PbrBundle {
        mesh: asset_server.load("cube.obj"),
        material: materials.add(Color::rgb(0.8, 0.7, 0.6).into()),
        ..Default::default()
    })
    .insert(Cube);
}

fn cube_movement(time: Res<Time>, mut query: Query<(&Cube, &mut Transform)>) {
    // 立方体の動きを制御するロジックをここに追加
    for (_, mut transform) in query.iter_mut() {
        // 例: 立方体を左に動かす
        transform.translation.x += time.delta_seconds();
    }
}

fn cube_rotation(time: Res<Time>, mut query: Query<&mut Transform, With<Cube>>) {
    // 立方体の回転を制御するロジックをここに追加
    for mut transform in query.iter_mut() {
        // 例: 立方体を回転させる
        let rotation_speed = 1.0; // 回転速度
        transform.rotate(Quat::from_rotation_y(rotation_speed * time.delta_seconds()));
    }
}

fn input_system(input: Res<Input<KeyCode>>, mut query: Query<&mut Transform, With<Cube>>) {
    for mut transform in query.iter_mut() {
        // キーボードの左右矢印キーで立方体を動かす
        if input.pressed(KeyCode::Left) {
            transform.translation.x -= 1.0;
        }
        if input.pressed(KeyCode::Right) {
            transform.translation.x += 1.0;
        }
    }
}

fn load_asset_config(file_path: &str) -> Vec<AssetConfig> {
    // JSONファイルからアセットの設定を読み込む
    let json_string = std::fs::read_to_string(file_path).expect("Failed to read JSON file");
    let asset_config: Vec<AssetConfig> = serde_json::from_str(&json_string).expect("Failed to parse JSON");
    asset_config
}

fn setup(mut commands: Commands) {
    // カメラの追加
    commands.spawn_bundle(OrthographicCameraBundle::new_2d());
}

fn load_assets(mut asset_server: ResMut<AssetServer>, asset_config: Res<Vec<AssetConfig>>) {
    // JSONから読み込んだアセットのパスをAssetServerに登録
    for config in asset_config.iter() {
        asset_server.load(&config.path);
    }
}

fn setup_assets(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    asset_config: Res<Vec<AssetConfig>>,
) {
    // JSONから読み込んだアセットの設定を元にマテリアルを作成
    for config in asset_config.iter() {
        let texture_handle = asset_server.load(&config.path);
        let color = Color::rgb(config.color[0], config.color[1], config.color[2]);
        let material = materials.add(texture_handle.into());
        commands.insert_resource(material);
    }
}

fn spawn_cube(mut commands: Commands, material: Res<Assets<ColorMaterial>>) {
    // 立方体の追加
    commands.spawn_bundle(PbrBundle {
        mesh: StandardMesh::cube(),
        material: material.get_handle(),
        ..Default::default()
    })
    .insert(Cube);
}

use bevy::prelude::*;

#[wasm_bindgen]
pub fn main() {
    App::build()
        .add_startup_system(setup.system())
        // 他のシステムを追加
        .add_plugins_with(DefaultPlugins, |group| {
            group.disable(bevy::log::LogPlugin);
        })
        .run();
}

fn setup(commands: &mut Commands) {
    // 初期化ロジックを追加
}
