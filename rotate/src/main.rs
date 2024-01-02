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

mod sample_constant; // 別ファイル参照

use crate::sample_constant::{DEG, X, Y, Z}; // 別ファイルからの値の引用
const  PI: f64 = 3.14;
const RAD: f64 = DEG / 180. * PI;

#[allow(dead_code)] // constant `` is never used の解消
fn main() { // main fn の中にないと異名関数は動作しない
    fn rotate2dim() {
        let x: f64 = X * RAD.cos() - Y * RAD.sin();
        let y: f64 = X * RAD.sin() + Y * RAD.cos();

        println!("{} {}", x, y);
    }

    fn rotate3dim() {
        let x1: f64 = X;
        let x2: f64 = Y * RAD.cos() - Z * RAD.sin();
        let x3: f64 = Y * RAD.sin() + Z * RAD.cos();

        let y1: f64 = X * RAD.cos() + Z * RAD.sin();
        let y2: f64 = Y;
        let y3: f64 = -X * RAD.sin() + Z * RAD.cos();

        let z1: f64 = X * RAD.cos() - Y * RAD.sin();
        let z2: f64 = X * RAD.sin() + Y * RAD.cos();
        let z3: f64 = Z;

        println!(
            "{} {} {} {} {} {} {} {} {}",
            x1, x2, x3, y1, y2, y3, z1, z2, z3
        );
    }
}
