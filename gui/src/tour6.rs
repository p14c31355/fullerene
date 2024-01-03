fn say_it_loud(msg:&str){
  println!("{}!!!",msg.to_string().to_uppercase());
}


fn main() {
  let a: &'static str = "こんにちは 🦀";
  println!("{} {}", a, a.len());
  let a: &'static str = "Ferrisは言う:\t\"こんにちは\"";
    println!("{}",a);
    let haiku: &'static str = "
    書いてみたり
    けしたり果ては
    けしの花
    - 立花北枝";
  println!("{}", haiku);


println!("こんにちは \
世界") // 世界の前にある間隔は無視されます
let a: &'static str = r#"
        <div class="advice">
            生文字列は様々な場面で役に立ちます。
        </div>
        "#;
    println!("{}", a);
    let a = "hi 🦀";
    println!("{}", a.len());
    let first_word = &a[0..2];
    let second_word = &a[3..7];
    // let half_crab = &a[3..5]; は失敗します。
    // Rust は無効な unicode 文字のスライスを受け付けません。
    println!("{} {}", first_word, second_word);
     // 文字をcharのベクトルとして集める
     let chars = "hi 🦀".chars().collect::<Vec<char>>();
     println!("{}", chars.len()); // should be 4
     // chars は 4 バイトなので、u32 に変換することができる
     println!("{}", chars[3] as u32);
     let mut helloworld = String::from("hello");
     helloworld.push_str(" world");
     helloworld = helloworld + "!";
     println!("{}", helloworld);

     // say_it_loudは&'static strを&strとして借用することができます
    say_it_loud("hello");
    // say_it_loudはStringを&strとして借用することもできます
    say_it_loud(&String::from("goodbye"));

    let helloworld = ["hello", " ", "world", "!"].concat();
    let abc = ["a", "b", "c"].join(",");
    println!("{}", helloworld);
    println!("{}",abc);
    let a = 42;
    let f = format!("secret to life: {}",a);
    println!("{}",f);

    fn resultOk() -> Result<(), std::num::ParseIntError> {
      let a = 42;
      let a_string = a.to_string();
      let b = a_string.parse::<i32>()?;
      println!("{} {}", a, b);
      Ok(())
  }
}