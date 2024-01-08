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


    const PI = 3.14159f32;

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
}
