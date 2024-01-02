#![allow(dead_code)] // この行でコンパイラのwaringsメッセージを止めます。

enum Species { Crab, Octopus, Fish, Clam }
enum PoisonType { Acidic, Painful, Lethal }
enum Size { Big, Small }
enum Weapon {
    Claw(i32, Size),
    Poison(PoisonType),
    None
}

enum Species {
    Crab,
    Octopus,
    Fish,
    Clam
}

struct SeaCreature {
    species: Species,
    name: String,
    arms: i32,
    legs: i32,
    weapon: Weapon,
}

struct SeaCreature {
    animal_type: String,
    name: String,
    arms: i32,
    legs: i32,
    weapon: String,
}

struct Location(i32, i32);
struct Marker;

fn main() {
    struct SeaCreature {
        // String は構造体である。
        animal_type: String,
        name: String,
        arms: i32,
        legs: i32,
        weapon: String,
    }

    // スタティックメソッドでStringインスタンスを作成する。
    let s = String::from("Hello world!");
    // インスタンスを使ってメソッド呼び出す。
    println!("{} is {} characters long.", s, s.len());

    // SeaCreatureのデータはスタックに入ります。
    let ferris = SeaCreature {
        // String構造体もスタックに入りますが、
        // ヒープに入るデータの参照アドレスが一つ入ります。
        animal_type: String::from("crab"),
        name: String::from("Ferris"),
        arms: 2,
        legs: 4,
        weapon: String::from("claw"),
    };

    let sarah = SeaCreature {
        animal_type: String::from("octopus"),
        name: String::from("Sarah"),
        arms: 8,
        legs: 0,
        weapon: String::from("none"),
    };
    
    println!(
        "{} is a {}. They have {} arms, {} legs, and a {} weapon",
        ferris.name, ferris.animal_type, ferris.arms, ferris.legs, ferris.weapon
    );
    println!(
        "{} is a {}. They have {} arms, and {} legs. They have no weapon..",
        sarah.name, sarah.animal_type, sarah.arms, sarah.legs
    );

    // これもスタックに入れられる構造体です。
    let loc = Location(42, 32);
    println!("{}, {}", loc.0, loc.1);

    let _m = Marker;

    let ferris = SeaCreature {
        species: Species::Crab,
        name: String::from("Ferris"),
        arms: 2,
        legs: 4,
        weapon: String::from("claw"),
    };

    match ferris.species {
        Species::Crab => println!("{} is a crab",ferris.name),
        Species::Octopus => println!("{} is a octopus",ferris.name),
        Species::Fish => println!("{} is a fish",ferris.name),
        Species::Clam => println!("{} is a clam",ferris.name),
    }

     // SeaCreatureのデータはスタックに入ります。
     let ferris = SeaCreature {
        // String構造体もスタックに入りますが、
        // ヒープに入るデータの参照アドレスが一つ入ります。
        species: Species::Crab,
        name: String::from("Ferris"),
        arms: 2,
        legs: 4,
        weapon: Weapon::Claw(2, Size::Small),
    };

    match ferris.species {
        Species::Crab => {
            match ferris.weapon {
                Weapon::Claw(num_claws,size) => {
                    let size_description = match size {
                        Size::Big => "big",
                        Size::Small => "small"
                    };
                    println!("ferris is a crab with {} {} claws", num_claws, size_description)
                },
                _ => println!("ferris is a crab with some other weapon")
            }
        },
        _ => println!("ferris is some other animal"),
    }
}
