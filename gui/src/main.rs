#![no_std]
#![no_main]

use avr_hal::prelude::*;
use avr_hal::pwm::Timer1Pwm;
use avr_hal::port::mode::{Output, Mode};
use panic_halt as _;

#[no_mangle]
pub extern "C" fn main() -> ! {
    let dp = avr_hal::target_device::Peripherals::take().unwrap();

    // PB1ピンを出力モードに設定
    let mut servo_pin: avr_hal::port::portb::PB1<Output> = dp.PORTB.pb1.into_output(&dp.DDRB);

    // PWM設定
    let pwm = Timer1Pwm::new(dp.TC1);

    // PWM初期化
    let mut pwm = pwm.into_output_pin(servo_pin, avr_hal::pwm::Prescaler::Prescale64);

    loop {
        // サーボモーターを0度に動かす
        pwm.set_duty(100); // サーボモーターの角度に合わせて調整
        avr_hal::delay::delay_ms(1000);

        // サーボモーターを90度に動かす
        pwm.set_duty(512);
        avr_hal::delay::delay_ms(1000);

        // サーボモーターを180度に動かす
        pwm.set_duty(920);
        avr_hal::delay::delay_ms(1000);
    }
}
