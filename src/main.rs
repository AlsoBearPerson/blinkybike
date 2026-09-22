#![no_std]
#![no_main]

mod animations;
mod bleuart;

use defmt::*;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::ws2812::{PioWs2812, PioWs2812Program};
use embassy_rp::{bind_interrupts, dma, peripherals, pio};
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::zerocopy_channel;
use embassy_time::{Duration, Instant, Ticker, Timer};
use fixed::types::*;
use panic_probe as _;
use smart_leds::RGB8;
use static_cell::StaticCell;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => pio::InterruptHandler<peripherals::PIO0>;
    PIO1_IRQ_0 => pio::InterruptHandler<peripherals::PIO1>;
    DMA_IRQ_0 => dma::InterruptHandler<peripherals::DMA_CH0>,
        dma::InterruptHandler<peripherals::DMA_CH1>,
        dma::InterruptHandler<peripherals::DMA_CH2>,
        dma::InterruptHandler<peripherals::DMA_CH3>;
});

const N1_LEDS: usize = 20;
const N2_LEDS: usize = 15;
const NUM_LEDS: usize = N1_LEDS + N2_LEDS;
type Pixbuf = [RGB8; NUM_LEDS];

#[embassy_executor::main(
    executor = "embassy_rp::executor::Executor",
    entry = "cortex_m_rt::entry"
)]
async fn main(spawner: Spawner) {
    info!("Start");
    let p = embassy_rp::init(Default::default());

    spawner.spawn(unwrap!(bleuart::run_ble(
        p.PIN_23, p.PIN_25, p.PIO1, p.PIN_24, p.PIN_29, p.DMA_CH2, p.DMA_CH3, spawner,
    )));

    // We use two tasks, one in charge of calculating pixels,
    // and one in charge of blinking out to the strip.
    // This is probably somewhat excessive, but provides nice separation,
    // and could be split across cores, if needed.
    // We tie them together via a zerocopy channel,
    // which in effect implements double-buffering,
    // while avoiding any unsafe/manual lifetime shenanigans.
    static RAW_BUF: StaticCell<[Pixbuf; 2]> = StaticCell::new();
    static CHANNEL: StaticCell<zerocopy_channel::Channel<'_, NoopRawMutex, Pixbuf>> =
        StaticCell::new();

    let buf = RAW_BUF.init([[RGB8::default(); _]; _]);
    let chan = CHANNEL.init(zerocopy_channel::Channel::new(buf));
    let (send, recv) = chan.split();

    spawner.spawn(unwrap!(producer(send)));
    spawner.spawn(unwrap!(consumer(
        recv, p.PIO0, p.DMA_CH0, p.DMA_CH1, p.PIN_16, p.PIN_17
    )));
}

const GAMMA8: [u8; 256] = color_parse::srgb_to_linear_table!();

const fn gamma(c: RGB8) -> RGB8 {
    RGB8 {
        r: GAMMA8[c.r as usize],
        g: GAMMA8[c.g as usize],
        b: GAMMA8[c.b as usize],
    }
}

fn scale(factor: u8, mut colors: [RGB8; 256]) -> [RGB8; 256] {
    for c in &mut colors {
        *c /= factor;
    }
    return colors;
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

// A walk through Oklch(0.7, 0.15, x) with small tweaks,
// see extras/wheelscan.py
pub const WHEEL_OKLAB_07:   [RGB8; 16] = colors_linear![ #E8729B, #ED7472, #E97C48, #DB8912, #C19905, #A1A717, #74B34C, #30BA79, #00B8A1, #01B4BF, #05AFDC, #43A5F6, #7A98FC, #A28BF3, #C17FDE, #D977C0];
}
use crate::flags::*;

// Returns a value ramping across [0, 1), repeating every `secs`.
fn time_strobe(secs: u64) -> I8F24 {
    // This implementation is a bit rough, there's likely a better way to do this.

    // embassy-rp as time driver configures a 1MHz tick rate.
    // We know: ticks since startup won't overflow a u64 (takes ~600k years)
    // We assume: width as ticks won't overflow a u32 (true for secs <= 4294).
    let now = Instant::now().as_ticks();
    let width = Instant::from_secs(secs).as_ticks();
    // We need the fractional part of division, and width isn't a nice power of 2.
    // The common trick (which fixed uses) is to go up one integer width:
    // given x: u32, y: u32, you can calculate
    // let wide_div: u64 = ((x as u64) << 32) / y;
    // but with our inputs already u64, that calls for a U128/U64 division.
    // Which, on a 32-bit CPU, might be slow.
    //
    // So instead, we first do a U64%U64 modulo.
    let remainder = now % width;
    // Now, since we're assuming width fits in u32, remainder will too.
    let width = U32F0::from_num(width);
    let remainder = U32F0::from_num(remainder);
    // Then we do a wide_div between u32s, only doing a U64/U32 division.
    let result: U32F32 = remainder.wide_div(width);
    // Finally we can truncate to our desired precision.
    return result.to_num();
}

const ANIM_SECS: u64 = 60;

#[embassy_executor::task]
async fn producer(mut s: zerocopy_channel::Sender<'static, NoopRawMutex, Pixbuf>) {
    let mut ticker = Ticker::every(Duration::from_millis(10));
    const LINEAR: animations::LinRev<N1_LEDS, N2_LEDS> = animations::LinRev;
    let mut anim = animations::ManyAnim::new(
        animations::smoothwheel(LINEAR, &WHEEL_OKLAB_07),
        animations::rgbwheel(LINEAR),
        animations::flag(LINEAR, &FLAG_BI),
        animations::flag(LINEAR, &FLAG_LESSBEANS),
    );
    anim.set_fade(U8F8::lit("0.125"));
    let mut last_strobe: I8F24 = I8F24::ZERO;
    loop {
        let now: I8F24 = time_strobe(ANIM_SECS);
        if last_strobe > now {
            // Each time the strobe walks backwards,
            // we assume we just completed a cycle, so cycle animations.
            anim.advance();
        }
        last_strobe = now;

        run_send(&mut s, |buf| {
            anim.frame(now, buf);
        })
        .await;
        ticker.next().await;
    }
}

#[embassy_executor::task]
async fn consumer(
    mut r: zerocopy_channel::Receiver<'static, NoopRawMutex, Pixbuf>,
    pio: embassy_rp::Peri<'static, embassy_rp::peripherals::PIO0>,
    dma1: embassy_rp::Peri<'static, embassy_rp::peripherals::DMA_CH0>,
    dma2: embassy_rp::Peri<'static, embassy_rp::peripherals::DMA_CH1>,
    pin1: embassy_rp::Peri<'static, embassy_rp::peripherals::PIN_16>,
    pin2: embassy_rp::Peri<'static, embassy_rp::peripherals::PIN_17>,
) {
    let Pio {
        mut common,
        sm0,
        sm1,
        ..
    } = Pio::new(pio, Irqs);
    let program = PioWs2812Program::new(&mut common);
    let mut ws2812_1 = PioWs2812::new(&mut common, sm0, dma1, Irqs, pin1, &program);
    let mut ws2812_2 = PioWs2812::new(&mut common, sm1, dma2, Irqs, pin2, &program);

    loop {
        run_recv(&mut r, async |buf| {
            let f1 = ws2812_1.write_slice(unwrap!(buf.first_chunk::<N1_LEDS>()));
            let f2 = ws2812_2.write_slice(unwrap!(buf.last_chunk::<N2_LEDS>()));
            embassy_futures::join::join(f1, f2).await;
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
// Awaits an available send slot, runs the passed closure on it,
// then automatically marks the value as ready.
async fn run_send<M, T, R, F>(s: &mut zerocopy_channel::Sender<'static, M, T>, f: F) -> R
where
    F: FnOnce(&mut T) -> R,
    M: RawMutex,
{
    let mut slot = s.send().await;
    let result = f(&mut *slot);
    slot.send_done();
    return result;
}

// As above, but for receiving, and takes an async closure.
// This one can get more finicky with lifetimes,
// but seems to be fine for simple cases.
async fn run_recv<M, T, F, R>(r: &mut zerocopy_channel::Receiver<'static, M, T>, f: F) -> R
where
    F: AsyncFnOnce(&mut T) -> R,
    M: RawMutex,
{
    let mut slot = r.receive().await;
    let result = f(&mut *slot).await;
    slot.receive_done();
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
