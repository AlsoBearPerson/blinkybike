#![no_std]
#![no_main]

use defmt::*;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::ws2812::{PioWs2812, PioWs2812Program};
use embassy_rp::{bind_interrupts, dma, peripherals, pio};
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::zerocopy_channel;
use embassy_time::{Duration, Ticker, Timer};
use panic_probe as _;
use smart_leds::RGB8;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => pio::InterruptHandler<peripherals::PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<peripherals::DMA_CH0>;
});

const NUM_LEDS: usize = 20;
type Pixbuf = [RGB8; NUM_LEDS];

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(spawner: Spawner) {
    info!("Start");
    let p = embassy_rp::init(Default::default());

    static RAW_BUF: StaticCell<[Pixbuf; 2]> = StaticCell::new();
    static CHANNEL: StaticCell<zerocopy_channel::Channel<'_, NoopRawMutex, Pixbuf>> =
        StaticCell::new();

    let buf = RAW_BUF.init([[RGB8::default(); _]; _]);
    let chan = CHANNEL.init(zerocopy_channel::Channel::new(buf));
    let (send, recv) = chan.split();

    spawner.spawn(producer(send).unwrap());
    spawner.spawn(consumer(recv, p.PIO0, p.DMA_CH0, p.PIN_16).unwrap());
}

const GAMMA8: [u8; 256] = color_parse::srgb_to_linear_table!();

const fn gamma(c: RGB8) -> RGB8 {
    RGB8 {
        r: GAMMA8[c.r as usize],
        g: GAMMA8[c.g as usize],
        b: GAMMA8[c.b as usize],
    }
}

const fn wheel(pos: u8) -> RGB8 {
    // We do a color wheel in 3 sections, ramping linearly.
    // Our range of 0..256 divides by 3 almost evenly,
    // we end up returning pure red for pos=0 and pos=255.
    if pos < 85 {
        let d = 3 * pos;
        return RGB8::new(255 - d, d, 0);
    } else if pos < 170 {
        let d = 3 * (pos - 85);
        return RGB8::new(0, 255 - d, d);
    } else {
        let d = 3 * (pos - 170);
        return RGB8::new(d, 0, 255 - d);
    }
}

fn flag<const N: usize>(colors: [RGB8; N]) -> [RGB8; 256] {
    let mut result = [RGB8::default(); 256];
    for i in 0..192 {
        result[i] = colors[i * N / 192];
    }
    return result;
}

fn scale(factor: u8, mut colors: [RGB8; 256]) -> [RGB8; 256] {
    for c in &mut colors {
        *c /= factor;
    }
    return colors;
}

fn rampbow() -> [RGB8; 256] {
    let mut result = [RGB8::default(); _];
    for i in 0..256 {
        result[i] = wheel(255 - i as u8);
    }
    return result;
}
fn hsvbow() -> [RGB8; 256] {
    let mut result = [RGB8::default(); 256];
    for i in 0..256 {
        result[i] = gamma(smart_leds::hsv::hsv2rgb(smart_leds::hsv::Hsv {
            hue: 255 - i as u8,
            sat: 255,
            val: 255,
        }));
    }
    return result;
}

#[rustfmt::skip]
#[allow(unused)]
mod flags {
use smart_leds::RGB8;
use color_parse::colors_linear;
pub const FLAG_PRIDE:        [RGB8; 6] = colors_linear![ #E40303, #FF8C00, #FFED00, #008026, #004CFF, #732982];
pub const FLAG_TRAAANS:      [RGB8; 5] = colors_linear![ #5BCEFA, #F5A9B8, #FFFFFF, #F5A9B8, #5BCEFA];
pub const FLAG_LESSBEANS:    [RGB8; 7] = colors_linear![ #D52D00, #EF7627, #FF9A56, #FFFFFF, #D162A4, #B55690, #A30262];
pub const FLAG_BI:           [RGB8; 5] = colors_linear![ #D60270, #D60270, #9B4F96, #0038A8, #0038A8];
pub const FLAG_NUMEROUSBEES: [RGB8; 4] = colors_linear![ #FCF434, #FFFFFF, #9C59D1, #2C2C2C];
pub const FLAG_PAN:          [RGB8; 3] = colors_linear![ #FF218C, #FFD800, #21B1FF];
}
use flags::*;

#[embassy_executor::task]
async fn producer(mut s: zerocopy_channel::Sender<'static, NoopRawMutex, Pixbuf>) {
    let ctable = scale(
        4,
        flag(FLAG_PRIDE),
        //hsvbow(),
        //rampbow()
    );
    let mut ticker = Ticker::every(Duration::from_millis(10));
    loop {
        for offset in 0..256 {
            run_send(&mut s, |buf| {
                for i in 0..NUM_LEDS {
                    buf[i] = ctable[(offset - 256 * i / NUM_LEDS) & 255];
                }
            })
            .await;
            ticker.next().await;
        }
    }
}

#[embassy_executor::task]
async fn consumer(
    mut r: zerocopy_channel::Receiver<'static, NoopRawMutex, Pixbuf>,
    pio: embassy_rp::Peri<'static, embassy_rp::peripherals::PIO0>,
    dma: embassy_rp::Peri<'static, embassy_rp::peripherals::DMA_CH0>,
    pin: embassy_rp::Peri<'static, embassy_rp::peripherals::PIN_16>,
) {
    let Pio {
        mut common, sm0, ..
    } = Pio::new(pio, Irqs);
    let program = PioWs2812Program::new(&mut common);
    let mut ws2812 = PioWs2812::new(&mut common, sm0, dma, Irqs, pin, &program);

    loop {
        run_recv(&mut r, async |buf| {
            ws2812.write(buf).await;
        })
        .await;
        // Compensate for a minor bug in ws2812.write:
        // It waits for DMA to finish, then another 55us for ws2812 latch time.
        // However, the DMA will likely finish as the final word gets loaded
        // into the tx fifo, before the state machine actually sends them out.
        // So we wait another 30us (800kHz * 24 pixels) to catch that last transfer.
        // No need to hold on to the buffer for that wait, though.
        Timer::after(Duration::from_micros(30)).await;
    }
}

// A small ergonomics helper for zerocopy channels:
// Awaits an available send slot, runs the passed closure,
// then automatically marks the value as ready.
async fn run_send<M, T, R, F>(s: &mut zerocopy_channel::Sender<'static, M, T>, f: F) -> R
where
    F: FnOnce(&mut T) -> R,
    M: RawMutex,
{
    let buf: &mut T = s.send().await;
    let result = f(buf);
    s.send_done();
    return result;
}

// As above, but for receiving, and takes an async closure.
// This one can get more finicky with lifetimes,
// but seems to be fine for simple cases.
async fn run_recv<M, T, F, R>(r: &mut zerocopy_channel::Receiver<'static, M, T>, f: F) -> R
where
    F: AsyncFnOnce(&mut T) -> R,
    M: RawMutex
{
    let v = r.receive().await;
    let result = f(v).await;
    r.receive_done();
    return result;
}

// Program metadata for `picotool info`.
// This isn't needed, but it's recommended to have these minimal entries.
#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 4] = [
    embassy_rp::binary_info::rp_program_name!(c"BlinkyBike"),
    embassy_rp::binary_info::rp_program_description!(
        c"WIP: Using a pi pico 2W to drive a string of ws2812 LEDs."
    ),
    embassy_rp::binary_info::rp_cargo_version!(),
    embassy_rp::binary_info::rp_program_build_attribute!(),
];
