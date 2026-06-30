#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::gpio::{self, Input, Pull};
use embassy_time::Timer;
use gpio::{Level, Output};
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    let mut led = Output::new(p.PIN_25, Level::Low);
    let mut async_input = Input::new(p.PIN_16, Pull::None);

    loop {
        info!("wait_for_high. Turn on LED");
        led.set_high();

        async_input.wait_for_high().await;

        info!("done wait_for_high. Turn off LED");
        led.set_low();

        Timer::after_secs(2).await;
    }
}
