#![allow(dead_code)] // この行でコンパイラのwaringsメッセージを止めます。

enum Species { Crab, Octopus, Fish, Clam } // enum(列挙型)は1行でも改行もOK
enum PoisonType { Acidic, Painful, Lethal }
enum Size { Big, Small }
enum Weapon {
    Claw(i32, Size),
    Poison(PoisonType),
    None
}

trait SeaCreatureTrait {
    fn species(&Species) -> String;
    fn name(&self) -> &str;
    fn arms(&self) -> i32;
    fn legs(&self) -> i32;
    fn weapon(&self) -> String;
}

struct Location(i32, i32); // 座標
struct Marker;

struct BagOfHolding<T> {
    item: T,
}

enum Item {
    Inventory(String),
    None, // None は項目がないことを表す
}

struct BagOfHolding {
    item: Item,
}

// 部分的に定義された構造体型
struct BagOfHolding<T> {
    // パラメータ T を渡すことが可能
    item: Option<T>,
}


fn main() {

    let x = 13; // x の型を推論
    println!("{}", x);

    let x: f64 = 3.14159; // x の型を指定
    println!("{}", x);

    let x;
    x = 0; // 宣言の後で初期化（あまり使われません）
    println!("{}", x);

    let mut x = 42;
    println!("{}", x);
    x = 13;
    println!("{}", x);

    let x = 12; // デフォルトでは i32
    let a = 12u8; //型を変形可能
    let b = 4.3; // デフォルトでは f64
    let c = 4.3f32;
    let bv = true;
    let t = (13, false);
    let sentence = "hello world!";
    println!(
        "{} {} {} {} {} {} {} {}",
        x, a, b, c, bv, t.0, t.1, sentence
    );

    let a = 13u8;
    let b = 7u32;
    let c = a as u32 + b; //型変換しながらの代入
    println!("{}", c);

    let t = true;
    println!("{}", t as u8); //型変換しながらの表示

    let nums: [i32; 3] = [1, 2, 3];
    println!("{:?}", nums);
    println!("{}", nums[1]);


    pub(crate) const PI = 3.14159f32;

    fn applepi() {
        println!(
            "ゼロからアップル {} を作るには、まず宇宙を創造する必要があります。",
            PI
        );
    }

    fn add(x: i32, y: i32) -> i32 {
        x + y
    }

    fn print_add() {
        println!("{}", add(42, 13));
    }

    fn swap(x: i32, y: i32) -> (i32, i32) {
        (y, x)
    }

    fn return_result() {
        // 戻り値をタプルで返す
        let result = swap(123, 321);
        println!("{} {}", result.0, result.1);

        // タプルを2つの変数に分解
        let (a, b) = swap(result.0, result.1);
        println!("{} {}", a, b);

    }
    
    fn print_nothing() {
        let a = make_nothing();
        let b = make_nothing2();

        // 空を表示するのは難しいので、
        // a と b のデバッグ文字列を表示
        //デバッグ用に空関数を用意することがある
        println!("a の値: {:?}", a);
        println!("b の値: {:?}", b);
    }

    fn branch() {
        let x = 42;
        if x < 42 {
            println!("42 より小さい");
        } else if x == 42 {
            println!("42 に等しい");
        } else {
            println!("42 より大きい");
        }
    }

    fn r#loop() {
        let mut x = 0;
        loop {
            x += 1;
            if x == 42 {
                break;
            }
        }
        println!("{}", x);
    }
        // while : なるまでやる
    fn whiloop() {
        let mut x = 0;
        while x != 42 {
            x += 1;
        }
    }

    fn loop_iterator() {
        //0~4
        for x in 0..5 {
            println!("{}", x);
        }

        for x in 0..=5 {
            //0~5
            println!("{}", x);
        }
    }

    fn r#match() {
        let x = 42;

        match x {
            0 => {
                println!("found zero");
            }
            // 複数の値にマッチ
            1 | 2 => {
                println!("found 1 or 2!");
            }
            // 範囲にマッチ
            3..=9 => {
                println!("found a number 3 to 9 inclusively");
            }
            // マッチした数字を変数に束縛
            matched_num @ 10..=100 => {
                println!("found {} number between 10 to 100!", matched_num);
            }
            // どのパターンにもマッチしない場合のデフォルトマッチが必須
            _ => {
                println!("found something else!");
            }
        }
    }

    fn r#break() {
        let mut x = 0;
        let v = loop {
            x += 1;
            if x == 13 {
                break "13 を発見";
            }
        };
        println!("loop の戻り値: {}", v);
    }

    fn example() -> i32 {
        let x = 42;
        // Rust の三項式
        let v = if x < 42 { -1 } else { 1 };
        println!("if より: {}", v);

        let food = "ハンバーガー";
        let result = match food {
            "ホットドッグ" => "ホットドッグです",
            // 単一の式で値を返す場合、中括弧は省略可能
            _ => "ホットドッグではありません",
        };
        println!("食品の識別: {}", result);

        let v = {
            // ブロックのスコープは関数のスコープから分離されている
            let a = 1;
            let b = 2;
            a + b
        };
        println!("ブロックより: {}", v);

        // Rust で関数の最後から値を返す慣用的な方法
        v + 4
    }

    fn print_example() {
        println!("関数より: {}", example());
    }

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

    // 注意: ジェネリック型を使用すると、型はコンパイル時に作成される。
    // ::<> (turbofish) で明示的に型を指定
    let i32_bag = BagOfHolding::<i32> { item: 42 };
    let bool_bag = BagOfHolding::<bool> { item: true };
    
    // ジェネリック型でも型推論可能
    let float_bag = BagOfHolding { item: 3.14 };
    
    // 注意: 実生活では手提げ袋を手提げ袋に入れないように
    let bag_in_bag = BagOfHolding {
        item: BagOfHolding { item: "boom!" },
    };

    println!(
        "{} {} {} {}",
        i32_bag.item, bool_bag.item, float_bag.item, bag_in_bag.item.item
    );

    // 注意: i32 が入るバッグに、何も入っていません！
    // None からは型が決められないため、型を指定する必要があります。
    let i32_bag = BagOfHolding::<i32> { item: None };

    if i32_bag.item.is_none() {
        println!("バッグには何もない！")
    } else {
        println!("バッグには何かある！")
    }

    let i32_bag = BagOfHolding::<i32> { item: Some(42) };

    if i32_bag.item.is_some() {
        println!("バッグには何かある！")
    } else {
        println!("バッグには何もない！")
    }

    // match は Option をエレガントに分解して、
    // すべてのケースが処理されることを保証できます！
    match i32_bag.item {
        Some(v) => println!("バッグに {} を発見！", v),
        None => println!("何も見付からなかった"),
    }

    let result = do_something_that_might_fail(12);

    // match は Result をエレガントに分解して、
    // すべてのケースが処理されることを保証できます！
    match result {
        Ok(v) => println!("発見 {}", v),
        Err(e) => println!("Error: {}",e),
    }

    // 型を明示的に指定
    let mut i32_vec = Vec::<i32>::new(); // turbofish <3
    i32_vec.push(1);
    i32_vec.push(2);
    i32_vec.push(3);

    // もっと賢く、型を自動的に推論
    let mut float_vec = Vec::new();
    float_vec.push(1.3);
    float_vec.push(2.3);
    float_vec.push(3.4);

    // きれいなマクロ！
    let string_vec = vec![String::from("Hello"), String::from("World")];

    for word in string_vec.iter() {
        println!("{}", word);
    }
}

// main は値を返しませんが、エラーを返すことがあります！
fn main() -> Result<(), String> {
    let result = do_something_that_might_fail(12);

    match result {
        Ok(v) => println!("発見 {}", v),
        Err(_e) => {
            // エラーをうまく処理
            
            // 何が起きたのかを説明する新しい Err を main から返します！
            return Err(String::from("main で何か問題が起きました！"));
        }
    }

    // Result の Ok の中にある unit 値によって、
    // すべてが正常であることを表現していることに注意してください。
    Ok(())
}

fn do_something_that_might_fail(i: i32) -> Result<f32, String> {
    if i == 42 {
        Ok(13.0)
    } else {
        Err(String::from("正しい値ではありません"))
    }
}

fn main() -> Result<(), String> {
    // コードが簡潔なのに注目！
    let v = do_something_that_might_fail(42)?;
    println!("発見 {}", v);
    Ok(())
}

// unwrap(); は想定していない結果が出力されたときプログラムを panic させて強制的に終了させるため良い手段とはいえない

/*
fn main() -> Result<(), String> {
    // 簡潔ですが、値が存在することを仮定しており、
    // すぐにダメになる可能性があります。
    let v = do_something_that_might_fail(42).unwrap();
    println!("発見 {}", v);
    
    // パニックするでしょう！
    let v = do_something_that_might_fail(1).unwrap();
    println!("発見 {}", v);
    
    Ok(())

}
*/