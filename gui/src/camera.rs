/*
Copyright 2023 YoshitakaNaraoka

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

      http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/

use bevy::prelude::*;
use crate::shape::UVSphere;

fn main() {
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
}
