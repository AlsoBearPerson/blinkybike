#![no_std]
#![no_main]

mod animations;
mod bleuart;
mod utils;

use core::sync::atomic::AtomicBool;
use core::sync::atomic::Ordering::Relaxed;

use defmt::*;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::ws2812::{PioWs2812, PioWs2812Program};
use embassy_rp::{bind_interrupts, dma, peripherals, pio};
use embassy_sync::blocking_mutex::raw::{NoopRawMutex, RawMutex};
use embassy_sync::zerocopy_channel;
use embassy_time::{Duration, Instant, Timer};
use fixed::{Saturating, traits::Fixed, types::*};
use heapless::format;
use panic_probe as _;
use smart_leds::RGB8;
use static_cell::StaticCell;

use crate::animations::CanAdvance;
use crate::bleuart::UartMessage;
use crate::flags::*;
use crate::utils::{AtomicFixed, AtomicI8F24, AtomicU8F8};

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
    spawner.spawn(unwrap!(process_commands()));

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

#[embassy_executor::task]
async fn process_commands() -> ! {
    let r = bleuart::get_receiver();
    loop {
        let v = r.receive().await;
        for cmd in v.split('\n') {
            match handle_command(cmd).await {
                Ok(x) if x.is_empty() => (),
                Ok(reply) => bleuart::try_send(reply),
                Err(_) => warn!("Reply dropped, too large"),
            };
        }
    }
}

const FADE_FACTOR: U8F8 = U8F8::lit("1.5");
const SPEED_FACTOR: I8F24 = I8F24::lit("1.5");

// Handling heapless::String is a tad inconvenient,
// as there's no infallible way to convert string literals to heapless strings.
// So for implementation convenience, we drag the Result type around,
// expecting no error unless something went very wrong.
async fn handle_command(cmd: &str) -> Result<UartMessage, heapless::CapacityError> {
    match cmd {
        "B--" => shrink_cmd(&FADE_LEVEL, FADE_FACTOR, "Darker", "Too dark!"),
        "B++" => grow_cmd(
            &FADE_LEVEL,
            FADE_FACTOR,
            U8F8::ONE,
            "Brighter",
            "Max bright!",
        ),
        "S--" => {
            if clear_paused() {
                return "Unpaused.".try_into();
            }
            shrink_cmd(&TIME_MULT, SPEED_FACTOR, "Slower", "Too slow!")
        }
        "S++" => {
            if clear_paused() {
                return "Unpaused.".try_into();
            }
            grow_cmd(
                &TIME_MULT,
                SPEED_FACTOR,
                10.into(),
                "Faster",
                "Ludicrous speed!",
            )
        }
        "PAUSE" => {
            let val = flip_paused();
            if val {
                "Unpaused.".try_into()
            } else {
                "Paused.".try_into()
            }
        }
        "NEXT" => {
            let val = set_fastforward();
            if val {
                "Stuck?".try_into()
            } else {
                "Go next.".try_into()
            }
        }
        _ => "???".try_into(),
    }
}

fn shrink_cmd<F: AtomicFixed>(
    holder: &F,
    factor: F::Value,
    msg: &str,
    underflow_msg: &str,
) -> Result<UartMessage, heapless::CapacityError> {
    let cur = holder.get() / factor;
    if cur > 0 {
        holder.set(cur);
        changemsg(msg, cur)
    } else {
        underflow_msg.try_into()
    }
}

fn grow_cmd<F: AtomicFixed>(
    holder: &F,
    factor: F::Value,
    max: F::Value,
    msg: &str,
    overflow_msg: &str,
) -> Result<UartMessage, heapless::CapacityError> {
    let cur = holder.get();
    let mut val = cur.saturating_mul(factor);
    if val == cur && val < F::Value::MAX {
        // Handle edge case of 0x0.01 * 0x1.8 truncating back to 0x0.01
        val += F::Value::DELTA;
    }
    let result = if val < max {
        changemsg(msg, val)
    } else {
        val = max;
        overflow_msg.try_into()
    };
    holder.set(val);
    result
}

fn changemsg(
    msg: &str,
    val: impl Fixed + defmt::Format,
) -> Result<UartMessage, heapless::CapacityError> {
    format!("{}->{:X}", msg, val).or_else(|_| {
        warn!("Fmt fail/overflow: {:?} / {:?}", msg, val);
        msg.try_into()
    })
}

#[allow(unused)]
const GAMMA8: [u8; 256] = color_parse::srgb_to_linear_table!();
#[allow(unused)]
const fn gamma(c: RGB8) -> RGB8 {
    RGB8 {
        r: GAMMA8[c.r as usize],
        g: GAMMA8[c.g as usize],
        b: GAMMA8[c.b as usize],
    }
}

mod flags {
    use color_parse::colors_linear;
    use smart_leds::RGB8;
    pub const FLAG_PRIDE: [RGB8; 6] =
        colors_linear![ #E40303, #FF8C00, #FFED00, #008026, #004CFF, #732982];
    pub const FLAG_TRAAANS: [RGB8; 5] =
        colors_linear![ #5BCEFA, #F5A9B8, #FFFFFF, #F5A9B8, #5BCEFA];
    pub const FLAG_LESSBEANS: [RGB8; 7] =
        colors_linear![ #D52D00, #EF7627, #FF9A56, #FFFFFF, #D162A4, #B55690, #A30262];
    pub const FLAG_BI_CYCLE: [RGB8; 5] =
        colors_linear![ #D60270, #D60270, #9B4F96, #0038A8, #0038A8];
    // Welp, good luck displaying "black" on a self-lit medium.
    // Gotta have this one, though.
    pub const FLAG_NUMEROUSBEES: [RGB8; 4] = colors_linear![ #FCF434, #FFFFFF, #9C59D1, #2C2C2C];
    pub const FLAG_PANPANPAN: [RGB8; 3] = colors_linear![ #FF218C, #FFD800, #21B1FF];

    // Long story.
    pub const FLAG_GLETSCHER: [RGB8; 6] =
        colors_linear![ #005CB9, #F38B00, #F4CD00, #FFFFFF, #009BDE, #005CB9 ];

    // A walk through Oklch(0.7, 0.15, x) with small tweaks,
    // see extras/wheelscan.py
    pub const WHEEL_OKLAB_07: [RGB8; 16] = colors_linear![ #E8729B, #ED7472, #E97C48, #DB8912, #C19905, #A1A717, #74B34C, #30BA79, #00B8A1, #01B4BF, #05AFDC, #43A5F6, #7A98FC, #A28BF3, #C17FDE, #D977C0];
}

static FADE_LEVEL: AtomicU8F8 = AtomicU8F8::new(U8F8::lit("0.5"));
static TIME_MULT: AtomicI8F24 = AtomicI8F24::new(I8F24::lit("0.1"));
static PAUSED: AtomicBool = AtomicBool::new(false);
fn is_paused() -> bool {
    PAUSED.load(Relaxed)
}
// Negates pausedness, returning whether we were paused BEFORE the call.
fn flip_paused() -> bool {
    PAUSED.fetch_not(Relaxed)
}
// Ensures PAUSED=false, returning whether we were paused before.
fn clear_paused() -> bool {
    PAUSED.swap(false, Relaxed)
}
static FAST_FORWARD: AtomicBool = AtomicBool::new(false);
// Ensures FAST_FORWARD=false, returning whether it was true before.
fn clear_fastforward() -> bool {
    FAST_FORWARD.swap(false, Relaxed)
}
// Ensures FAST_FORWARD=true, returning whether it was already true before.
fn set_fastforward() -> bool {
    FAST_FORWARD.swap(true, Relaxed)
}

#[embassy_executor::task]
async fn producer(mut s: zerocopy_channel::Sender<'static, NoopRawMutex, Pixbuf>) {
    const LINEAR: animations::LinRev<N1_LEDS, N2_LEDS> = animations::LinRev;
    let mut a0 = animations::many_flag(
        LINEAR,
        &[
            &FLAG_GLETSCHER,
            &FLAG_BI_CYCLE,
            &FLAG_PRIDE,
            &FLAG_TRAAANS,
            &FLAG_LESSBEANS,
            &FLAG_NUMEROUSBEES,
            &FLAG_PANPANPAN,
        ],
    );
    let a1 = animations::smoothwheel(LINEAR, &WHEEL_OKLAB_07);
    let a2 = animations::rgbwheel(LINEAR);
    loop {
        push(&mut s, &a0, 2).await;
        a0.advance();
        push(&mut s, &a1, 3).await;
        push(&mut s, &a0, 2).await; // Repeat the flag slot, we have many to show.
        a0.advance();
        push(&mut s, &a2, 3).await;
    }
}

// Convert a Duration into fractional fixed-point seconds
fn duration_to_secs(d: Duration) -> I8F24 {
    let mut ticks = d.as_ticks();
    let mut ticks_per_s = Duration::from_secs(1).as_ticks();
    const U32MAX: u64 = u32::MAX as u64;
    // unlikely: If we're dealing with huge values, trim insignificant bits
    while ticks > U32MAX || ticks_per_s > U32MAX {
        ticks >>= 1;
        ticks_per_s >>= 1;
    }
    if ticks_per_s == 0 {
        // Many ticks, left side.
        return I8F24::MAX;
    }
    let ticks = U32F0::from_num(ticks);
    let ticks_per_s = U32F0::from_num(ticks_per_s);
    // While I64F0::wide_div would work, that calls for a 128-bit division,
    // which sounds rather slow on a 32-bit CPU, so let's stick to 32->64 bits.
    let wide_result: U32F32 = ticks.wide_div(ticks_per_s);
    return wide_result.saturating_to_num();
}

async fn push<A: animations::Animation>(
    s: &mut zerocopy_channel::Sender<'static, NoopRawMutex, Pixbuf>,
    anim: &A,
    max_iters: usize,
) {
    let mut last_time = Instant::now();
    let mut time_strobe: Saturating<I8F24> = I8F24::ZERO.into();
    let mut state: A::State = Default::default();
    let mut iter_count = 0;
    loop {
        if clear_fastforward() {
            return;
        }
        let now = Instant::now();
        let since_last = now.saturating_duration_since(last_time);
        last_time = now;

        if !is_paused() {
            let since_last: I8F24 = duration_to_secs(since_last).saturating_mul(TIME_MULT.get());
            time_strobe += since_last;
            if time_strobe.0 > 1 {
                iter_count += 1;
                if iter_count >= max_iters {
                    return;
                }
                time_strobe.0 = I8F24::ZERO;
            }
        }

        run_send(s, |buf| {
            anim.frame(&mut state, time_strobe.0, buf);
            // Fade each LED by multiplying each color channel with the requested fade.
            // For fades greater than 1, this may saturate some channels at 255,
            // which will likely alter the resulting hue.
            let fade: U8F8 = FADE_LEVEL.get();
            for led in buf {
                *led = led
                    .iter()
                    .map(|c| fade.wide_mul(U8F8::from(c)).saturating_to_num())
                    .collect();
            }
        })
        .await;

        // Pad with a sleep, if needed (and always yields at least once).
        // Differs from Ticker in that there's no accumulated catch-up:
        // Wait is relative to last cycle end, regardless of prior cycles.
        const MIN_LOOP_TIME: Duration = Duration::from_millis(10);
        Timer::at(last_time + MIN_LOOP_TIME).await;
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
// marks the value as ready once it returns.
async fn run_send<M, T, R, F>(s: &mut zerocopy_channel::Sender<'static, M, T>, f: F) -> R
where
    F: FnOnce(&mut T) -> R,
    M: RawMutex,
{
    let mut slot = s.send().await;
    let result = f(&mut *slot); // Explicitly unwrap the smart pointer
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
