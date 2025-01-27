 // 共通箇所の列挙
trait FooTrait {
  fn process(&self) -> String;
  fn increment(&mut self);
}

// trait で実装した共通箇所にさまざまな動作を付与していく
impl FooTrait for Foo {
    fn process(&self) -> String {
        format!("{}",self.x)
    }

    fn increment(&mut self) {
        self.x +=1;
    }
}

// 最終的に関数で実行するのだが，その関数は trait と impl により簡略化することが可能
fn do_something<T: FooTrait>(f: &mut T) {
    println!("{}", f.process());
    f.increment();
}


fn do_something(a: &Foo) -> &i32 {
    return &a.x;
}

// 引数 foo と戻り値はライフタイムを共有
fn do_something<'a>(foo: &'a Foo) -> &'a i32 {
    return &foo.x;
}

// foo_b と戻り値はライフタイムを共有
// foo_a のライフタイムは別
fn do_something<'a, 'b>(foo_a: &'a Foo, foo_b: &'b Foo) -> &'b i32 {
    println!("{}", foo_a.x);
    println!("{}", foo_b.x);
    return &foo_b.x;
}

struct Foo<'a> {
    i:&'a i32
}

struct SeaCreature {
    pub name: String,
    noise: String,
  }
  trait LoudNoiseMaker: NoiseMaker {
    fn make_alot_of_noise(&self) {
        self.make_noise();
        self.make_noise();
        self.make_noise();
    }
    
    fn dynamic_make_noise(noise_maker: &dyn NoiseMaker) {
      // we don't know the real type
      noise_maker.make_noise();
    }
    
    fn generic_make_noise<T>(creature: &T)
    where
      T: NoiseMaker
    {
      // we know the real type at compile-time
      creature.make_noise();
    }
  
    fn make_noise(&self);
    
  }
  
  impl LoudNoiseMaker as NoiseMaker for SeaCreature {
    fn make_noise(&self) {
        println!("{}", &self.get_sound());
    }
  
    pub fn get_sound(&self) -> &str {
      &self.noise
    }
  }
  struct Ocean {
    animals: Vec<Box<dyn NoiseMaker>>,
  }

fn main() {
    // 構造体をインスタンス化し、変数に束縛してメモリリソースを作成
    let foo = Foo { x: 42 };
    // foo は所有者

    let foo_a = Foo { x: 42 };
    let foo_b = Foo { x: 13 };

    println!("{}", foo_a.x);

    println!("{}", foo_b.x);
    // foo_b はここでドロップ
    // foo_a はここでドロップ

    let foo = Foo { bar: Bar { x: 42 } };
    println!("{}", foo.bar.x);
    // foo が最初にドロップ
    // 次に foo.bar がドロップ

    let foo = Foo { x: 42 };
    // foo の所有権は do_something に移動
    do_something(foo);
    // foo は使えなくなる

    let foo = do_something();
    // foo は所有者になる
    // 関数のスコープの終端により、foo はドロップ

    let foo = Foo { x: 42 };
    let f = &foo;
    println!("{}", f.x);
    // f はここでドロップ
    // foo はここでドロップ

    let mut foo = Foo { x: 42 };
    let f = &mut foo;

    // 失敗: do_something(foo) はここでエラー
    // foo は可変に借用されており移動できないため

    // 失敗: foo.x = 13; はここでエラー
    // foo は可変に借用されている間は変更できないため

    f.x = 13;
    // f はここから先では使用されないため、ここでドロップ
    
    println!("{}", foo.x);
    
    // 可変な借用はドロップされているため変更可能
    foo.x = 7;
    
    // foo の所有権を関数に移動
    do_something(foo);

    let mut foo = 42;
    let f = &mut foo;
    let bar = *f; // 所有者の値を取得
    *f = 13;      // 参照の所有者の値を設定
    println!("{}", bar);
    println!("{}", foo);

    let mut foo = Foo { x: 42 };
    do_something(&mut foo);
    // 関数 do_something で可変な参照はドロップされるため、
    // 別の参照を作ることが可能
    do_something(&mut foo);
    // foo はここでドロップ

    let mut foo = Foo { x: 42 };
    let x = &mut foo.x;
    *x = 13;
    // x はここでドロップされるため、不変な参照が作成可能
    let y = do_something(&foo);
    println!("{}", y);
    // y はここでドロップ
    // foo はここでドロップ

    let mut foo = Foo { x: 42 };
    let x = &mut foo.x;
    *x = 13;
    // x はここでドロップされるため、不変な参照が作成可能
    let y = do_something(&foo);
    println!("{}", y);
    // y はここでドロップ
    // foo はここでドロップ

    let foo_a = Foo { x: 42 };
    let foo_b = Foo { x: 12 };
    let x = do_something(&foo_a, &foo_b);
    // ここから先は foo_b のライフタイムしか存在しないため、
    // foo_a はここでドロップ
    println!("{}", x);
    // x はここでドロップ
    // foo_b はここでドロップ

     // スタティック変数は関数スコープでも定義可能
     static mut SECRET: &'static str = "swordfish";

     // 文字列リテラルは 'static ライフタイム
     let msg: &'static str = "Hello World!";
     let p: &'static f64 = &PI;
     println!("{} {}", msg, p);
 
     // ルールを破ることはできますが、それを明示する必要があります。
     unsafe {
         // 文字列リテラルは 'static なので SECRET に代入可能
         SECRET = "abracadabra";
         println!("{}", SECRET);
     }

     let x = 42;
    let foo = Foo {
        i: &x
    };
    println!("{}",foo.i);

    let ferris = SeaCreature {
        name: String::from("Ferris"),
        noise: String::from("blub"),
      };
    
      let sarah = SeaCreature {
        name: String::from("Sarah"),
        noise: String::from("swish"),
      };
    
      let ocean = Ocean {
        animals: vec![Box::new(ferris), Box::new(sarah)],
      };
    
      println!("{}", creature.get_sound());
      creature.make_noise();
      creature.make_alot_of_noise();
      static_make_noise(&creature);
      dynamic_make_noise(&creature);
      generic_make_noise(&creature);
    
      for a in ocean.animals.iter() {
        a.make_noise();
    
      };
}
