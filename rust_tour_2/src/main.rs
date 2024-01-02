fn main() {

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

}