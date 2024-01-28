#![no_std]
#![no_main]

use panic_halt as _;

use avr_hal::prelude::*;
use avr_hal::atmega328p;
use avr_hal::port::mode::{Input, Output};
use avr_hal::port::portb::PB1;
use avr_hal::delay::Delay;

use avr_hal::pwm::Pwm1;

// サーボモーターの角度範囲 (この範囲はサーボモーターによって異なります)
const SERVO_MIN: u16 = 1000;
const SERVO_MAX: u16 = 2000;

// サーボモーターを動かす関数
fn move_servo<T: Pwm1>(servo: &mut T, angle: u16) {
    let duty_cycle = ((angle - SERVO_MIN) * (servo.get_max_duty() - servo.get_min_duty()))
        / (SERVO_MAX - SERVO_MIN)
        + servo.get_min_duty();
    
    servo.set_duty(duty_cycle);
}

// メイン関数
#[avr_device::entry]
fn main() -> ! {
    let dp = atmega328p::Peripherals::take().unwrap();
    let mut delay = Delay::new();

    // PB1を出力モードに設定
    let mut servo_pin: PB1<Output> = dp.PORTB.pb1.into_output(&dp.DDRB);

    // PWMを初期化
    let mut pwm = dp.TC1.pwm(
        servo_pin.into_output(&dp.DDRB),
        avr_hal::pwm::Timer1Pwm::top::<avr_hal::time::Milliseconds>(20),
    );

    loop {
        // サーボモーターを0度に動かす
        move_servo(&mut pwm, 1000);
        delay.delay_ms(1000);

        // サーボモーターを90度に動かす
        move_servo(&mut pwm, 1500);
        delay.delay_ms(1000);

        // サーボモーターを180度に動かす
        move_servo(&mut pwm, 2000);
        delay.delay_ms(1000);
    }
}
