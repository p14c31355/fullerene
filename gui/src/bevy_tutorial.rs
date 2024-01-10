use bevy::prelude::*;
use bevy::ecs::system::SystemParam; // 要インポート
use bevy::tasks::ComputeTaskPool; // 要 import

// Components
// Component を derive した struct や enum が Component として使用できる
#[derive(Component)]
struct Person;
#[derive(Component)]
struct Name(String);

// Component を Bundle としてまとめて定義する
// Bundle を定義するには derive(Bundle) が必要
#[derive(Default, Bundle)]
struct PlayerStatus {
    hp: PlayerHp,
    mp: PlayerMp,
    xp: PlayerXp,
}

// Bundle は入れ子にすることもできる
#[derive(Default, Bundle)]
struct PlayerBundle {
    name: PlayerName,
    position: Position,
    _marker: Player,
    // Bundle を入れ子にするには #[bundle] が必要
    #[bundle]
    status: PlayerStatus,
}

fn add_entities(mut commands: Commands) {
  // Player Bundle を持った Entity を生成する
  commands.spawn_bundle(PlayerBundle::default());
  // Enemy の Entity を生成する
  // タプルで Component をまとめると Bundle と解釈される
  commands.spawn_bundle((Enemy, Position { x: -1.0, y: -2.0 }));
}

fn my_system(
  resource: Res<MyResource>,        // ResourceA にイミュータブルアクセス
  query: Query<&MyComponent>,       // ComponentA をクエリしてアクセス
  mut commands: Commands,           // Commands を使って要素の生成･削除
  event_writer: EventWriter<Data>,  // Event を送信
  (ra, mut rb): (Res<ResourceA>, ResMut<ResourceB>),  // タプルでグループ化
) {
// do something
}

App::new()
    
    .add_system(second.label("second")) // second という label をつける
    .add_system(first.before("second")) // first は second より前に
    .add_system(fourth.label("fourth").after("second")) // fourth は second より後に
    .add_system(third.after("second").before("fourth")) // third は second より後 fourth より前に
    .run();

    App::new()
    
    .add_system_set( // second と third をまとめて SystemSet にする
        SystemSet::new()
            
            .label("second and third") // label をつけておく
            
            .after("first") // first よりも後に second と third を実行する
            
            .with_system(second.label("second")) // ここにも label をつけておく
            
            .with_system(third.after("second")), // third は second より後に
    )
    
    .add_system(first.label("first").before("second and third")) // first は second and third より前に
    
    .add_system(fourth.after("second and third")) // fourth は second and third より後に
    .run();

    use anyhow::Result;
    
    // 処理の結果を後段の System へ転送する
    fn parse_number() -> Result<()> {
        let s = "number".parse::<i32>()?; // Err
        Ok(())
    }
    
    // 前段の System から送られてきた Result が Err だったら処理する
    fn handle_error(In(result): In<Result<()>>) {
        if let Err(e) = result {
            println!("parse error: {:?}", e);
        }
    }
    
    fn main() {
        App::new().add_system(
                
                parse_number.chain(handle_error), // chain() で後段の System を登録する
            )
            .run();
    }

    

    // SystemParam は System の引数にできるものは何でも持つことができる
    #[derive(SystemParam)]
    struct MySystemParam<'w, 's> {
        query: Query<'w, 's, (Entity, &'static MyComponent)>,
        resource: ResMut<'w, MyResource>,
        local: Local<'s, usize>,
    }
    
    #[derive(SystemParam)]
    struct MySystemParam2<'w, 's> {
        resource: ResMut<'w, MyResource>,
        // 's を満たすために PhantomData が必要
        #[system_param(ignore)]
        _secret: PhantomData<&'s ()>,
    }
    
    // SystemParam は直接 System の引数にすることができる
    fn query_system_param(mut my_system_param: MySystemParam) {
        // ..
    }

    fn my_system(mut q: Query<&mut MyComponent>) {
      // iter() iter_mut() でイテレートして処理
      for c in q.iter_mut() {
          // すべての MyComponent をミュータブルにイテレートして処理
      }
  
      // get() get_mut() で Entity を指定して取得
      if let Ok(c) = q.get_mut(entity) {
          // 指定した Entity が見つかれば処理
      }
  
      // Entity がひとつだけなのであれば、直接取得できる
      let c = q.single();
  }

  fn my_system(
    q_a: Query<&CompA>,                    // A を持つ Entity
    mut q_b: Query<&mut CompB>,            // B を持つ Entity (Mutable)
    q_ac: Query<(&CompA, &CompC)>,         // A と C を両方持つ Entity
    q_ad: Query<(&CompA, Option<&CompD>)>, // A と持っていれば D
    q_ae: Query<(&CompA, Entity)>,
)         // A とその Entity ID)
{
    for a in q_a.iter() {
        println!("{:?}", a);
    }

    let mut b = q_b.single_mut().unwrap();
    // do something with b
    println!("{:?}", b);

    for (a, c) in q_ac.iter() {
        println!("{:?}, {:?}", a, c);
    }

    for (a, d) in q_ad.iter() {
        println!("{:?}, {:?}", a, d);
    }

    for (a, e) in q_ae.iter() {
        println!("{:?}, {:?}", a, e);
    }
}

fn my_system(
  q_a_wc: Query<&CompA, With<CompC>>,                       // A with C
  q_a_woc: Query<&CompA, Without<CompC>>,                   // A w/o C
  q_ac_wd: Query<(&CompA, &CompC), With<CompD>>,            // A + C with D
  q_a_wc_wod: Query<&CompA, (With<CompC>, Without<CompD>)>, // A with C && w/o D
  q_a_wc_wd: Query<&CompA, Or<(With<CompC>, With<CompD>)>>, // A with C || with D
) {
  // ...
}

// NG: ミュータブルな A の Query が複数存在する
fn my_system(
  mut q_a_wb: Query<&mut CompA, With<CompB>>, // A with B (Mutable)
  mut q_a_wc: Query<&mut CompA, With<CompC>>, // A with C (Mutable)
) {
  // ...
}

// OK
fn my_system(
  mut q: QuerySet<(
      QueryState<&mut CompA, With<CompB>>, // A with B (Mutable)
      QueryState<&mut CompA, With<CompC>>, // A with C (Mutable)
  )>,
) {
  for mut a in q.q0().iter_mut() {
      // QueryState<&mut CompA, With<CompB>> にアクセス
  }

  for mut a in q.q1().iter_mut() {
      // QueryState<&mut CompA, With<CompC>> にアクセス
  }
}

fn add_two_comp(query: Query<&MyComp>) {
  let mut iter = query.iter_combinations();
  while let Some([MyComp(c1), MyComp(c2)]) = iter.fetch_next() {
      println!("{} + {} = {}", c1, c2, c1 + c2);
  }
}

// parallel_iterator を使用するには、ComputeTaskPool が必要
fn add_one(pool: Res<ComputeTaskPool>, mut query: Query<&mut MyComp>) {
    const BATCH_SIZE: usize = 100;
    query.par_for_each_mut(&pool, BATCH_SIZE, |mut my_comp| {
        my_comp.0 += 1;
    });
}

fn main() {
  App::new()
      // ...
      .init_resource::<MyResourceA>()   // Default or FromWorld で初期化されて追加
      .insert_resource(MyResourceB(5))  // 手動で初期化して追加
      // ...
      .run();
}

fn my_system(mut commands: Commands) {
  commands.insert_resource(MyResource(3));
  commands.remove_resource::<MyResource>();
}

// Default で初期化される Resource
#[derive(Default)]
struct MyFirstCounter(usize);

// FromWorld で初期化される Resource
struct MySecondCounter(usize);

impl FromWorld for MySecondCounter {
    // ここでは ECS World のすべての要素にアクセスすることが可能
    fn from_world(world: &mut World) -> Self {
        let count = world.get_resource::<MyFirstCounter>().unwrap();
        MySecondCounter(count.0) // MyFirstCounter の値で初期化
    }
}

// 初期化できない Resource
struct MyThreshold(usize);

// System
fn count_up(
  thresh: Res<MyThreshold>,                        // イミュータブルに参照
  mut first_counter: ResMut<MyFirstCounter>,       // ミュータブルに参照
  second_counter: Option<ResMut<MySecondCounter>>, // 存在するかわからない Resource
) {
  if let Some(mut second_counter) = second_counter {
      // MySecondCounter が存在したら何かする
  }
}

#[derive(Component)]
struct MyTimer(Timer);

fn setup(mut commands: Commands) {
    // MyTimer を 2.0 秒ごとに繰り返すように設定
    commands.spawn().insert(MyTimer(Timer::from_seconds(2.0, true)));
}

// MyTimer に経過時間を Time Resource を使って適用し、指定時間が経過したかをチェックする
fn print_timer(time: Res<Time>, mut query: Query<&mut MyTimer>) {
    let mut my_timer = query.single_mut();
    if my_timer.0.tick(time.delta()).just_finished() {
        println!("Tick");
    }
}

fn main() {
    App::new()
        .add_plugins(MinimalPlugins)
        .add_startup_system(setup)
        .add_system(print_timer)
        .run();
}

// Local<T> には Default の実装が必要
#[derive(Default)]
struct MyCounter(usize);

fn count_up_1(mut counter: Local<MyCounter>) {
    // ここの counter と
}
fn count_up_2(mut counter: Local<MyCounter>) {
    // ここの counter は別物
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_system(count_up_1)
        // .config() を使って手動で Local<MyCounter> を初期化する
        .add_system(count_up_2.config(|params| {
            params.0 = Some(MyCounter(0)); // 0 番目の引数を初期化したいので params.0
        }))
        .run();
}

fn setup(mut commands: Commands) {
  // Entity の生成と Component/Bundle の追加
  let a = commands
      .spawn()        // Entity (EntityCommands) を生成
      .insert(CompA)  // Component を追加
      .id();          // id() で Entity を取得できる
  let ab = commands
      .spawn_bundle((CompA, CompB))  // Bundle を持った Entity を生成
      .id();
  let abc = commands
      .spawn()
      .insert(CompA)
      .insert_bundle((CompB, CompC)) // Bundle を追加 (タプルは Bundle)
      .id();

  // spawn_batch() で Bundle へのイテレータから複数の Entity を一度に生成できる
  commands.spawn_batch(vec![
      (CompA, CompB, CompC),
      (CompA, CompB, CompC),
      (CompA, CompB, CompC),
  ]);

  // Entity の Component を削除
  commands.entity(abc).remove::<CompB>();

  // Entity の削除
  commands.entity(a).despawn();
  commands.entity(ab).despawn();
  commands.entity(abc).despawn();

  // Resource の追加と削除
  commands.insert_resource(MyResource);
  commands.remove_resource::<MyResource>();
}

// 送受信する Event
// 内部に送受信したい様々なデータをもたせる
struct MyEvent(String);

/// Event を送信する System
/// EventWriter を使って MyEvent を送信する
fn event_write(mut event_writer: EventWriter<MyEvent>) {
    event_writer.send(MyEvent("Hello, event!".to_string()))
}

/// Event を受信する System
/// EventReader のイテレータを使って受信 Event を処理する
fn event_read(mut event_reader: EventReader<MyEvent>) {
    for e in event_reader.iter() {
        let msg = &e.0;
        println!("received event: {}", msg);
    }
}

fn main() {
    App::new()
        .add_event::<MyEvent>() // Event を App に登録する必要がある
        .add_system(event_write)
        .add_system(event_read)
        .run();
}

// Plugin にまとめたい様々な要素
struct MyResource;
struct MyEvent;

// 自作の Plugin
struct MyPlugin;

// 自作の Plugin に Plugin トレイトを実装すれば、Plugin として使用できる
// Plugin トレイトでは App Builder に必要な要素を追加するだけで良い
impl Plugin for MyPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(MyResource)
            .add_event::<MyEvent>()
            .add_startup_system(plugin_setup)
            .add_system(plugin_system);
    }
}

// Plugin にまとめたい startup_system
fn plugin_setup() {
    println!("MyPlugin's setup");
}

// Plugin にまとめたい system
fn plugin_system() {
    println!("MyPlugin's system");
}

fn main() {
    App::new()
        .add_plugin(MyPlugin) // Plugin を追加する
        .run();
}

use bevy::app::PluginGroupBuilder; // PluginGroup トレイトを実装するには追加が必要

// 自作の Plugin
struct FooPlugin;
struct BarPlugin;
impl Plugin for FooPlugin {}
impl Plugin for BarPlugin {}

// 自作の Plugin Group
struct MyPluginGroup;

impl PluginGroup for MyPluginGroup {
    fn build(&mut self, group: &mut PluginGroupBuilder) {
        group.add(FooPlugin).add(BarPlugin); // Plugin を group に追加
    }
}

fn main() {
    App::new()
        .add_plugins(MyPluginGroup) // PluginGroup を追加
        .run();
}

App::new()
    .add_plugins_with(DefaultPlugins, |plugins| {
        plugins
            .disable::<AudioPlugin>() // AudioPlugin を無効化
            .disable::<LogPlugin>() // LogPlugin を無効化
    })
    .run();

// State は enum として定義する (Debug, Clone, PartialEq, Eq, Hash が必要)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AppState { Menu, Game, End }

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        // State の初期値を登録して State を有効化する
        .add_state(AppState::Menu)
        // State に関係なく動作する System
        .add_system(count_frame)
        // AppState::Menu に入ったときのみ動作する System Set
        .add_system_set(
            SystemSet::on_enter(AppState::Menu).with_system(menu_on_enter)
        )
        // AppState::Game で毎フレーム動作する System Set
        .add_system_set(
            SystemSet::on_update(AppState::Game).with_system(game_on_update)
        )
        // AppState::End を終了するときのみ動作する System Set
        .add_system_set(
            SystemSet::on_exit(AppState::End).with_system(end_on_exit)
        )
        .run();
}

// System 内で State にアクセスするためには `State<T>` Resource を使う
fn count_frame(app_state: Res<State<AppState>>) {
  // current() で現在の State が取得できる
  match app_state.current() {
      // ...
  }
}

// System 内で State にアクセスするためには `State<T>` Resource を使う
fn menu_on_enter(mut app_state: ResMut<State<AppState>>) {
  app_state.set(AppState::Game).unwrap(); // Menu から Game へ Statを を変更
}

fn push_to_state_stack(mut app_state: ResMut<State<AppState>>) {
  // push() で State Stack に State を積む
  app_state.push(AppState::Paused).unwrap();
}

fn pop_from_state_stack(mut app_state: ResMut<State<AppState>>) {
  // pop() で State Stack から以前の状態に戻す
  app_state.pop().unwrap();
}


fn main() {
  App::new()
      // ...
      // AppState::Game が Inactive になる際に一度だけ呼ばれる
      .add_system_set(
          SystemSet::on_pause(AppState::Game)
              .with_system(game_on_pause)
      )
      // AppState::Game が Inactive でも Active でも毎フレーム呼ばれる
      // ただし Bevy 0.6.0 にはバグがあり、on_update() と同じ動作しかしてくれない
      // https://github.com/bevyengine/bevy/issues/3179
      .add_system_set(
          SystemSet::on_in_stack_update(AppState::Game)
              .with_system(game_on_in_stack_update),
      )
      // AppState::Game が Inactive なときだけ毎フレーム呼ばれる
      .add_system_set(
          SystemSet::on_inactive_update(AppState::Game)
              .with_system(game_on_inactive_update),
      )
      // AppState::Game が Active に戻る際に一度だけ呼ばれる
      .add_system_set(
          SystemSet::on_resume(AppState::Game)
            .with_system(game_on_resume)
      )
      .run();
}

fn esc_to_menu(
  mut keys: ResMut<Input<KeyCode>>,
  mut app_state: ResMut<State<AppState>>,
) {
  if keys.just_pressed(KeyCode::Escape) {
      app_state.set(AppState::MainMenu).unwrap();
      keys.reset(KeyCode::Escape);  // you should clear input by yourself
  }
}

use bevy::ecs::schedule::ShouldRun; // Run Criteria を使用するには追加が必要

// count が 100 より大きくなったときのみ ShouldRun::Yes を返す Run Criteria
fn my_run_criteria(mut count: Local<usize>) -> ShouldRun {
    if *count > 100 {
        *count = 0;
        ShouldRun::Yes
    } else {
        *count += 1;
        ShouldRun::No
    }
}

// my_run_criteria を適用する System
fn my_system() {
    println!("Hello, run criteria!");
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        // with_run_criteria() で Run Criteria を System に適用する
        .add_system(my_system.with_run_criteria(my_run_criteria))
        .run();
}

fn main() {
  App::new()
      .add_plugins(DefaultPlugins)
      .add_system(
          // my_run_criteria に Label をつけておく
          my_system.with_run_criteria(my_run_criteria.label("MyRunCriteria")),
      )
      // with_run_criteria に Label を指定すれば、その結果が使い回される
      .add_system_set(
          SystemSet::new()
              .with_run_criteria("MyRunCriteria")
              .with_system(my_system_a)
              .with_system(my_system_b),
      )
      .run();
}

// 独自の Label 型を定義する
#[derive(Debug, Clone, PartialEq, Eq, Hash, SystemLabel)]
struct MySecondSystemLabel;

fn main() {
    App::new()
        // 独自の Label 型は文字列の Label と同様に使用できる
        .add_system(second.label(MySecondSystemLabel))
        .add_system(first.before(MySecondSystemLabel))
        .add_system(third.after(MySecondSystemLabel))
        .run();
}

// 独自の Stage Label を定義する
#[derive(Debug, Clone, PartialEq, Eq, Hash, StageLabel)]
struct MyStage;

fn main() {
    App::new()
        // 独自の Stage を Label をつけて登録する (SystemStage::parallel() も使用可能)
        .add_stage_after(CoreStage::Update, MyStage, SystemStage::single_threaded())
        // CoreStage::Update で実行される System を登録
        .add_system(first)
        // MyStage に System を登録
        .add_system_to_stage(MyStage, second)
        .add_system_to_stage(MyStage, third)
        .run();
}

// Added<T> を Query Filter として、追加された Component を Query
fn print_added(query: Query<(Entity, &Counter), Added<Counter>>) {
  for (entity, counter) in query.iter() {
      println!("Added counter {}, count =  {}", entity.id(), counter.0);
  }
}

// Changed<T> を Query Filter として、変更された Component を Query
fn print_changed(query: Query<(Entity, &Counter), Changed<Counter>>) {
  for (entity, counter) in query.iter() {
      println!("Changed counter entity {} to {}", entity.id(), counter.0);
  }
}

// ChangeTrackers<T> を Query すれば、フィルタリングせずに追加･変更を検知できる
fn print_tracker(query: Query<(Entity, &Counter, ChangeTrackers<Counter>)>) {
  for (entity, counter, trackers) in query.iter() {
      if trackers.is_added() {
          println!("Tracker detected addition {}, {}", entity.id(), counter.0);
      }
      if trackers.is_changed() {
          println!("Tracker detected change {} to {}", entity.id(), counter.0);
      }
  }
}

// 存在するかわからない Resource の追加と変更を検知する
fn print_added_changed(counter: Option<Res<Counter>>) {
  if let Some(counter) = counter {
      // Counter が追加されたときに実行される
      if counter.is_added() {
          println!("Counter has added");
      }
      // Counter が変更されたときに実行される
      if counter.is_changed() {
          println!("Counter has change to {}", counter.0);
      }
  } else {
      println!("Counter has not found");
  }
}

// CoreStage::PostUpdate で MyComponent の削除を検知する
// RemovedComponents<T> でこれまでに削除された Component を検出できる
fn detect_removals(removals: RemovedComponents<MyComponent>) {
  for entity in removals.iter() {
      println!("MyComponent Entity {} has removed", entity.id());
  }
}

fn main() {
  App::new()
      .add_plugins(DefaultPlugins)
      // Component を追加する
      .add_startup_system(add_components)
      // CoreStage::Update で MyComponent が存在するときは削除する
      .add_system(remove_components_if_exist)
      // CoreStage::PostUpdate で MyComponent の削除を検知する
      .add_system_to_stage(CoreStage::PostUpdate, detect_removals)
      .run();
}

// 前フレームで Resource が存在したかを保存することで、次のフレームで削除を検知する
fn detect_removals(
  my_resource: Option<Res<MyResource>>,  // 存在するかわからないので Option
  mut my_resource_existed: Local<bool>,  // 前フレームでの有無をローカルに保存
) {
  if let Some(_) = my_resource {
      println!("MyResource exists");
      *my_resource_existed = true;
  } else if *my_resource_existed {
      println!("MyResource has removed");
      *my_resource_existed = false;
  }
}

#[derive(Component)]
struct MyParent(String);
#[derive(Component)]
struct MyChild(String);

fn setup(mut commands: Commands) {
    let parent = commands
        .spawn()
        .insert(MyParent("MyParent".to_string())) // Entity を MyParent とし、
        .with_children(|parent| {
            // Child として MyChild を生成
            parent.spawn().insert(MyChild("MyChild1".to_string()));
        })
        .id();

    // 別途 MyChild Entity を生成し
    let child = commands
        .spawn()
        .insert(MyChild("MyChild2".to_string()))
        .id();

    // MyParent の Entity ID を使って MyChild を Child として追加
    commands.entity(parent).push_children(&[child]);
}

// Child Entity の Query から、それぞれの Parent Entity を取得する
fn find_parent_from_children(
  q_child: Query<&Parent>,       // Parent Component を持つ Child Entity を Query
  q_my_parent: Query<&MyParent>, // MyParent を Query
) {
  // Child Entity が持っている Parent Component をイテレート
  for parent in q_child.iter() {
      // Parent Component は Entity (ID) を 0 番目の要素として持つので、
      // それを使って q_my_parent から MyParent を取得する
      let p = q_my_parent.get(parent.0).unwrap();
      println!("{}", p.0);
  }
}

// Parent Entity の Query から、Children Entity を取得する
fn find_children_from_parent(
  q_parent: Query<&Children>,  // Children Component を持つ Parent を Query
  q_my_child: Query<&MyChild>, // MyChild を Query
) {
  // Parent Entity が持っている Children Component をイテレート
  for children in q_parent.iter() {
      // Children Component は Child Entity ID の集合を持っているのでイテレート
      for &child in children.iter() {
          // Child Entity ID を使って、q_my_child から MyChild を取得する
          let my_child = q_my_child.get(child).unwrap();
          println!("{}", my_child.0);
      }
  }
}

// MyParent の Entity ID を Query し、それで MyParent を MyChild 含めて再帰的に破棄する
// MyParent は Children を持っているので、それを Query Filter として使う
fn clean_up(mut commands: Commands, query: Query<Entity, With<Children>>) {
  let e = query.single().unwrap();        // MyParent は唯一のはず
  commands.entity(e).despawn_recursive(); // MyParent を MyChild 含めて再帰的に破棄
}

// App を手動で Update する Runner をカスタマイズできる
fn my_runner(mut app: App) {
    println!("my_runner!");
    app.update();
}

fn hello_world() {
    println!("Hello, world!");
}

fn main() {
    App::new()
        // Custom Runner を適用する
        .set_runner(my_runner)
        .add_system(hello_world)
        .run();
}

use bevy::core::FixedTimestep; // 要 import

fn hello_world() {
    println!("hello world");
}

fn main() {
    App::new()
        .add_plugins(MinimalPlugins)
        .add_system_set(
            SystemSet::new()
                // FixedTimestep を Run Criteria として設定する
                // Time Step (== Frame Duration) を 0.5 に設定 (2 FPS)
                .with_run_criteria(FixedTimestep::step(0.5))
                .with_system(hello_world),
        )
        .run();
}

// 要 import
use bevy::app::{ScheduleRunnerPlugin, ScheduleRunnerSettings};

fn main() {
    App::new()
        // 1 秒ごとに System が実行されるように設定し、Plugin　を導入
        .insert_resource(ScheduleRunnerSettings::run_loop(Duration::from_secs_f64(
            1.0,
        )))
        .add_plugin(ScheduleRunnerPlugin::default())
        .add_system(hello_world)
        .run();
}

// Exclusive System を使うことで、App 内の World のすべての要素にアクセスできる
// https://docs.rs/bevy/latest/bevy/ecs/world/struct.World.html
fn my_exclusive_system(world: &mut World) {
  println!("Here is my exclusive system");
  world.insert_resource(MyResource);
  world.spawn().insert(MyComponent);
}

fn main() {
  App::new()
      .add_system(my_exclusive_system.exclusive_system())
      .run();
}

// スレッドプールの数を指定することができる
fn main() {
  App::new()
      .insert_resource(DefaultTaskPoolOptions::with_num_threads(4))
      .add_plugins(DefaultPlugins)
      .run();
}

App.new()
    // ...
    .add_plugin(LogPlugin::default())  // DefaultPlugin には同梱
    .insert_resource(ReportExecutionOrderAmbiguities)
    .run();

// cargo test を使って、System のテストを実行することが可能
// cargo test --test test_system

struct Counter(usize);

// テストされる System
fn count_up(mut query: Query<&mut Counter>) {
    for mut counter in query.iter_mut() {
        counter.0 += 1;
    }
}

#[test]
fn has_counted_up() {
    // World を自分で作り、Counter を持った Entity を生成する
    let mut world = World::default();
    let entity = world.spawn().insert(Counter(0)).id();

    // Stage を自分で作り、そこにテストすべき System を追加して実行する
    let mut update_stage = SystemStage::parallel();
    update_stage.add_system(count_up);
    update_stage.run(&mut world);

    // Component を取得して結果をテストする
    let count = world.get::<Counter>(entity).unwrap();
    assert_eq!(count.0, 1);
}

