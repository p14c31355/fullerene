use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::shape::UVSphere;

#[derive(Debug, Deserialize, Serialize)]
struct AssetConfig {
    path: String,
    color: [f32; 3],
}

// `Component` を実装した struct や enum が Component として使用可能
#[derive(Component)]
struct Position { x: f32, y: f32 }
struct PlayerName(String); // New Type を使って単純な String を Component として使える
struct Player; // 空の struct は Marker Component としても使える
struct Enemy;
struct Cube;
struct Entity(u64);

/*
Startup System
Rust の通常の関数が System として使用できる
App の開始時に一度だけ呼ばれる
Commands を使い、Person と Name を持った Entity を生成する
System
Rust の通常の関数が System として使用できる
Query を使って Component を取得し、それに対して処理を行う
*/

// App に System を登録し、Bevy の App を構築して Run する
fn main() {
    App::build()
    let asset_config: Vec<AssetConfig> = load_asset_config("assets.json"); // JSONファイルからアセットの設定を読み込む
        .add_startup_system(setup.system()) //各異名関数を main に投入している // 初期化時に一度だけ呼ばれる System
        .add_system(camera.system())
        .add_system(cube_movement.system())
        .add_system(cube_rotation.system())
        .add_system(input_system.system())
        .add_system(add_people.system())
        .add_system(greet_people.system())
        .add_system(add_entities.system())
        .add_startup_system(setup) // 起動時に一度だけ呼ばれる
        .add_system(hello_world) // 毎フレームよばれる
        .add_startup_system(move |world| setup.system().label("setup"))
        .add_startup_stage_after(stage::SETUP, "load_assets", SystemStage::single(load_assets.system()))
        .add_startup_system_to_stage("load_assets", move |world| setup_assets.system().label("setup_assets"))
        .add_startup_stage_after("load_assets", "spawn_cube", SystemStage::single(spawn_cube.system()))
        .add_plugins(DefaultPlugins) // 標準的な Bevy の機能を追加
        .insert_resource(MyResource)  // Global で唯一な Resource を追加
        .add_event::<MyEvent>()       // Event を追加
        .add_startup_system(setup)    
        .add_system(my_system)        // System を追加
        .run();
}

fn add_entity(mut commands: Commands) {
    let entity = commands
        .spawn()                           // Entity の生成
        .insert(Person)                    // Person Component の追加
        .insert(Name("Bevy".to_string()))  // Name Component の追加
        .id();                             // Entity を取得
  
    println!("Entity ID is {}", entity.id());
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

fn camera() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_systems(Startup, setup)
        .run();

    fn setup(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut transform: Transform, point_light: PointLight) {
        // カメラを追加
        commands.spawn(Camera3dBundle {
            transform: Transform::from_xyz(0.0, 6., 12.0)
                .looking_at(Vec3::new(0., 1., 0.), Vec3::Y),
        });
        // 光を追加
        commands.spawn(PointLightBundle {
            point_light: PointLight {
                intensity: 9000.0,
                range: 100.,
                shadows_enabled: true,
                transform: Transform::from_xyz(8.0, 16.0, 8.0),
            },
        });
        let sphere = meshes.add(UVSphere::default().into());
        commands.spawn(PbrBundle {
            mesh: sphere,
            // このxyzはカメラの向きと同じ
            transform: Transform::from_xyz(0.0, 1.0, 0.0),
            ..default()
        });
    }
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

fn add_people(mut commands: Commands) {
    commands.spawn().insert(Person).insert(Name("Rust".to_string()));
    commands.spawn().insert(Person).insert(Name("Bevy".to_string()));
    commands.spawn().insert(Person).insert(Name("Ferris".to_string()));
}

fn greet_people(query: Query<&Name, With<Person>>) {
    for name in query.iter() {
        println!("hello {}!", name.0);
    }
}

fn add_entities(mut commands: Commands) {
  // Player Entity を生成する
  commands
      .spawn()                                   // Entity を生成
      .insert(Player)                            // Player の Marker を追加
      .insert(Position::default())               // Position Component を追加
      .insert(PlayerName("Ferris".to_string())); // PlayerName を追加

  // Enemy Entity を生成する
  commands
      .spawn()                      // Entity を生成
      .insert(Enemy)                // Enemy の Marker を追加
      .insert(Position::default()); // Position Component を追加
}

fn setup () {
    println!("Hello from setup");
}
  
fn hello_world () {
    println!("Hello from system");
}  
