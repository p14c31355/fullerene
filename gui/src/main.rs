// Cargo.tomlに依存関係を追加する
// [dependencies]
// avr-hal = "0.6"
// avr-device = "0.7"
// panic-halt = "0.2"
// servo-sg90 = "0.1"

#![no_std]
#![no_main]

use panic_halt as _;

use avr_device::attiny85;
use avr_hal::prelude::*;
use avr_hal::clock::MHz8;
use avr_hal::delay::Delay;
use avr_hal::port::mode::Output;

use servo_sg90::Servo;

#[avr_device::entry]
fn main() -> ! {
    // ATTiny85のピンの設定
    let dp = attiny85::Peripherals::take().unwrap();
    let mut portb = dp.PORTB.split();

    // サーボモーターの制御ピンの設定
    let servo_pin = portb.pb0.into_output(&mut portb.ddr);

    // サーボモーターの初期化
    let mut servo = Servo::new(servo_pin);

    let mut delay = Delay::<MHz8>::new();

    loop {
        // サーボを0度に動かす
        servo.set_degrees(0);
        delay.delay_ms(1000);

        // サーボを90度に動かす
        servo.set_degrees(90);
        delay.delay_ms(1000);

        // サーボを180度に動かす
        servo.set_degrees(180);
        delay.delay_ms(1000);
    }
}
