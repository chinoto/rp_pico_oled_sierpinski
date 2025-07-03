#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::{gpio, spi::Spi};
use embedded_hal_async::delay::DelayNs;
use rand::prelude::*;
use ssd1306::{prelude::*, Ssd1306Async};
use {defmt_rtt as _, panic_probe as _};

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    info!("Initialize all the things!");
    let mut delay = embassy_time::Delay {};
    let p = embassy_rp::init(Default::default());

    let mut rst = gpio::Output::new(p.PIN_15, gpio::Level::High);
    let dc = gpio::Output::new(p.PIN_14, gpio::Level::Low);
    // Default frequency is 1Mhz, which seems to work for my display.
    let spi_config = embassy_rp::spi::Config::default();
    let spi = Spi::new_txonly(p.SPI0, p.PIN_2, p.PIN_3, p.DMA_CH0, spi_config);
    let cs = gpio::Output::new(p.PIN_13, gpio::Level::High);
    let spi = embedded_hal_bus::spi::ExclusiveDevice::new(spi, cs, delay.clone()).unwrap();

    let interface = SPIInterface::new(spi, dc);

    let mut display = Ssd1306Async::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    display.reset(&mut rst, &mut delay).await.unwrap();
    display.init().await.unwrap();

    info!("Initialization complete");

    // Every 1x8 set of pixels is a page and set_row only works on a page level
    // granularity, so for example, setting row 10 will actually put your cursor
    // at row 8, aka the second row of pages.

    // Clear the screen 8 rows at a time, skipping the dummy columns my display has.
    for y in (0..64).step_by(8) {
        info!("Clearing page row {}", y);
        display.set_row(y).await.unwrap();
        display.set_column(2).await.unwrap();
        display.draw(&[0; 128]).await.unwrap();
    }

    info!("Clearing complete");

    // Structure the buffer as sequence of columns so that pages can be read
    // contiguously for better cache performance.
    let mut buffer = [[0u8; 64]; 128];
    let corners = [Point(64, 0), Point(32, 63), Point(96, 63)];
    let mut cursor = corners[0];
    // thread_rng() doesn't exist for no_std, but rand provides SmallRng behind a feature flag.
    let rng = &mut rand::rngs::SmallRng::from_seed(*b"I am an adequate seed of chaos:)");

    loop {
        // I could simply resend the whole buffer or the single page in which a
        // pixel dies or lights up since there will only ever be one of each
        // (unless they overlap) with the current setup, but it was interesting
        // to come up with and I might decide to change the number of pixels
        // that are lit/die each frame. (Edit: I did 😁)

        // Track whether any of the 128 columns by 8 rows of pages (64 rows of pixels) need to be resent.
        let mut resend = [[false; 8]; 128];

        // Reduce every pixel's time to live.
        for (x, col) in buffer.iter_mut().enumerate() {
            for (y, ttl) in col.iter_mut().enumerate() {
                if *ttl == 0 {
                    continue;
                }
                *ttl -= 1;
                if *ttl == 0 {
                    // Divide by 8 to get the page row index
                    resend[x][y / 8] = true;
                }
            }
        }

        // Light up 10 random pixels of the Sierpinski triangle.
        for _ in 0..5 {
            cursor = cursor.midpoint(corners.choose(rng).unwrap());
            info!("Cursor: {},{}", cursor.0, cursor.1);
            let pixel = &mut buffer[cursor.0 as usize][cursor.1 as usize];
            if *pixel == 0 {
                resend[cursor.0 as usize][cursor.1 as usize / 8] = true;
            }
            // Live for the given number of frames.
            *pixel = 100;
        }

        // Find all the dirty pages to resend.
        for (x, y8) in resend.iter().enumerate().flat_map(|(x, pages)| {
            { pages.iter().enumerate() }.filter_map(move |(y8, dirty)| dirty.then_some((x, y8)))
        }) {
            // Accumulate bits in reverse to build the page.
            let page = { buffer[x][y8 * 8..(y8 + 1) * 8].iter().rev() }
                .fold(0u8, |acc, ttl| (*ttl > 0) as u8 + (acc << 1));

            // Skip the dummy columns.
            display.set_column(2 + x as u8).await.unwrap();
            display.set_row(y8 as u8 * 8).await.unwrap();
            display.draw(&[page; 1]).await.unwrap();
        }
        // 100 fps minus overhead
        delay.delay_ms(10).await;
    }
}

#[derive(Clone, Copy, Debug)]
struct Point(u16, u16);
impl Point {
    // Funny that this is the only thing I decided to abstract, yet only used it once...
    fn midpoint(&self, other: &Self) -> Self {
        let x = (self.0 + other.0) / 2;
        let y = (self.1 + other.1) / 2;
        Point(x, y)
    }
}
