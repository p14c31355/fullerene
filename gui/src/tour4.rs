struct BagOfHolding<T> {
    item: T,
}

enum Item {
    Inventory(String),
    // None は項目がないことを表す
    None,
}

struct BagOfHolding {
    item: Item,
}

// 部分的に定義された構造体型
struct BagOfHolding<T> {
    // パラメータ T を渡すことが可能
    item: Option<T>,
}

fn do_something_that_might_fail(i:i32) -> Result<f32,String> {
    if i == 42 {
        Ok(13.0)
    } else {
        Err(String::from("正しい値ではありません"))
    }
}

fn do_something_that_might_fail(i: i32) -> Result<f32, String> {
    if i == 42 {
        Ok(13.0)
    } else {
        Err(String::from("正しい値ではありません"))
    }
}

fn main() {
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

fn do_something_that_might_fail(i: i32) -> Result<f32, String> {
    if i == 42 {
        Ok(13.0)
    } else {
        Err(String::from("正しい値ではありません"))
    }
}

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
