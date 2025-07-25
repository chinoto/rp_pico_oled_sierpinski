#![no_std]
#![no_main]

use defmt::*;
use display_interface::AsyncWriteOnlyDataCommand;
use embassy_executor::Spawner;
use embassy_rp::{gpio, spi::Spi};
use embedded_hal_async::delay::DelayNs;
use rand::prelude::*;
use ssd1306::{mode::BufferedGraphicsModeAsync, prelude::*, Ssd1306Async};
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

    let mut drawer = FadingBufferedDrawer::new(display);

    let mut sierpinski_iterator = {
        let corners = [Point(64, 0), Point(32, 63), Point(96, 63)];
        let mut cursor = corners[0];
        // thread_rng() doesn't exist for no_std, but rand provides SmallRng behind a feature flag.
        let mut rng = rand::rngs::SmallRng::from_seed(*b"I am an adequate seed of chaos:)");
        core::iter::from_fn(move || {
            cursor = cursor.midpoint(corners.choose(&mut rng).unwrap());
            Some(cursor)
        })
    };

    loop {
        drawer.decrease_lifetimes();

        // Light up several random pixels of the Sierpinski triangle in this frame.
        for _ in 0..5 {
            let point = sierpinski_iterator.next().unwrap();
            drawer.set_pixel_lifetime(point, 100);
        }

        drawer.draw_pixels().await;
        // 100 fps minus overhead.
        delay.delay_ms(10).await;
    }
}

#[derive(Clone, Copy, Debug)]
struct Point(usize, usize);
impl Point {
    fn midpoint(&self, other: &Self) -> Self {
        let x = (self.0 + other.0) / 2;
        let y = (self.1 + other.1) / 2;
        Point(x, y)
    }
}

// The concrete type of DI (Display Interface) is huge, so we'll rely on
// generics and type inference to avoid specifying it since it isn't needed by
// the implementation blocks and allows changing which SPI controller is used.
// SIZE and MODE are specified here to avoid needing generics for them elsewhere when using this type.
struct FadingBufferedDrawer<DI> {
    display: Ssd1306Async<DI, DisplaySize128x64, BufferedGraphicsModeAsync<DisplaySize128x64>>,
    // Structure the pixel lifetimes as sequence of columns so that pages can be read
    // contiguously for better cache performance.
    pixel_lifetimes: [[u8; 64]; 128],
    // Tracks whether any of the 128 columns by 8 rows of pages (64 rows of pixels) need to be resent.
    resend: [[bool; 8]; 128],
}

impl<DI> FadingBufferedDrawer<DI> {
    fn new(
        display: Ssd1306Async<DI, DisplaySize128x64, BufferedGraphicsModeAsync<DisplaySize128x64>>,
    ) -> Self {
        Self {
            display,
            pixel_lifetimes: [[0; 64]; 128],
            resend: [[false; 8]; 128],
        }
    }
}

impl<DI: AsyncWriteOnlyDataCommand> FadingBufferedDrawer<DI> {
    fn decrease_lifetimes(&mut self) {
        for (x, col) in self.pixel_lifetimes.iter_mut().enumerate() {
            for (y, lifetime) in col.iter_mut().enumerate() {
                if *lifetime == 0 {
                    continue;
                }
                *lifetime -= 1;
                if *lifetime == 0 {
                    self.resend[x][y / 8] = true;
                }
            }
        }
    }

    fn set_pixel_lifetime(&mut self, Point(x, y): Point, new_lifetime: u8) {
        let lifetime = &mut self.pixel_lifetimes[x][y];
        // If the liveness of a pixel will change, mark its page as dirty.
        if (*lifetime != 0) != (new_lifetime != 0) {
            self.resend[x][y / 8] = true;
        }
        // Live for the given number of frames.
        *lifetime = new_lifetime;
    }

    async fn draw_pixels(&mut self) {
        // Find all the dirty pages to resend.
        for (x, col) in self.resend.iter().enumerate() {
            for y8 in { col.iter().enumerate() }.filter_map(|(y8, page)| page.then_some(y8)) {
                // Accumulate bits in reverse to build the page.
                let page = { self.pixel_lifetimes[x][y8 * 8..][..8].iter().rev() }
                    .fold(0u8, |acc, lifetime| (*lifetime > 0) as u8 + (acc << 1));

                // Skip the dummy columns.
                self.display.set_column(2 + x as u8).await.unwrap();
                self.display.set_row(y8 as u8 * 8).await.unwrap();
                self.display.draw(&[page; 1]).await.unwrap();
            }
        }

        self.resend = [[false; 8]; 128];
    }
}
