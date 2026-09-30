// Rust implementation of nRF's Nordic UART Service (NUS) over BLE,
// on embassy/trouble_host, for a rp235x.
//
// This provides a straightforward way to exchange messages with some central
// host, probably a smartphone. By using NUS, which is already supported by
// various BLE tinkering apps, we skip having to write our own control app,
// though this means we deal in text strings, rather than more structured types.
//
// Maybe in the future we'll rework this to provide a set of native GATT
// controls, so that relevant control knobs can be driven more directly
// by a native client. For now, we don't bother.
//
// The interface to the rest of the system is a pair of embassy_sync Channels,
// buffering messages received from, or to be sent to, a connected central host.
// These seem to act as a packet transport, not a byte stream,
// so newlines are probably optional, messages seem to arrive as sent.
//
// The only other public surface is the `run_ble()` task function,
// which should be spawned soon after HAL initialization.
// It takes ownership of some needed peripherals,
// and will drive hardware and software to provide connectivity,
// so long as the device and the executor spawning this task remain running:
// There are currently no controls exposed to pause or power down BLE again.

use cyw43::{Cyw43439, aligned_bytes, bluetooth::BtDriver};
use cyw43_pio::PioSpi;
use defmt::*;
use embassy_executor::Spawner;
use embassy_futures::select::select;
use embassy_rp::{
    Peri, dma,
    gpio::{Level, Output},
    peripherals::{DMA_CH2, DMA_CH3, PIN_23, PIN_24, PIN_25, PIN_29, PIO1},
    pio::Pio,
};
use embassy_sync::{blocking_mutex::raw::ThreadModeRawMutex, channel::Channel};
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;
use trouble_host::prelude::*;

// Our messages have a fixed maximum size,
// to ensure they can be allocated statically.
// Surely 100 characters should be good enough for anyone. Right?
pub const UART_MSG_SIZE: usize = 100;
pub type UartMessage = heapless::String<UART_MSG_SIZE>;

// Using ThreadModeRawMutex below is somewhat unsafe on the rp235x,
// as it won't handle multi-core access correctly.
// So far, we're only using core 0, so it's fine, for now,
// as long as we keep all access to these channels on the same core/executor.

/// Holds messages from NUS for processing in our application
static RX_CHAN: Channel<ThreadModeRawMutex, UartMessage, 10> = Channel::new();
/// Holds messages from our application to be sent over NUS
static TX_CHAN: Channel<ThreadModeRawMutex, UartMessage, 10> = Channel::new();

// Queue a message to send to a connected host, soon.
// If there is no host connected, we buffer a few messages,
// so a newly connected host may see a replay of what happened while it was away.
// Once the buffer is full, old messages are dropped,
// so the most recent messages eventually get sent.
pub fn try_send(v: UartMessage) {
    if TX_CHAN.is_full() {
        let _ = TX_CHAN.try_receive();
    }
    let _ = TX_CHAN.try_send(v);
}

pub fn get_receiver()
-> embassy_sync::channel::Receiver<'static, ThreadModeRawMutex, UartMessage, 10> {
    RX_CHAN.receiver()
}

// Manually place the large firmware blob into its own link section.
// This allows using "--skip-section .firmware" with probe-rs on reflashing,
// to save time when firmware blobs haven't changed.
// For best results, you'll also want to tweak memory.x to place this section
// at a fixed address, otherwise changes to program size will shift where
// the linker places this object, requiring reflashing anyway.
// A more elaborate trick would be to use rp235x's partition table feature,
// and have picotool upload firmware once. But that's more tools involved,
// and embassy-rp doesn't seem to have great support for navigating existing
// partitions, yet. Also, partitions would round up sizes to 4K multiples,
// unclear if the driver would be cool with that.
//
// Stable rust still doesn't like "const X: [u8; _] = ...",
// so we have to be a bit silly to accurately size the array.
const FW_LEN: usize = include_bytes!("../assets/cyw43-firmware/43439A0.bin").len();
#[unsafe(link_section = ".firmware")]
static FW: cyw43::Aligned<cyw43::A4, [u8; FW_LEN]> =
    // Can't use the aligned_bytes! macro, as that returns a reference to an
    // unsized type, which we couldn't deref and store into a static.
    // We need our static to be exactly the value in place, not merely
    // a reference to some compile-time constant, otherwise our link_section
    // override would merely place the reference, not its pointed-to value.
    cyw43::Aligned(*include_bytes!("../assets/cyw43-firmware/43439A0.bin"));
// We don't bother doing this with the other firmware blobs,
// they're comparatively tiny, at 6K for btfw, vs. the 226K of this chonker.

// Kicks off the entire BLE protocol stack.
// The pins listed here match the connections on a Pi Zero 2W dev board,
// and should not be changed without good reason.
// The PIO and DMA numbers are arbitrary, and could be switched.
// However, as embassy tasks can't be generic, these are just some designated
// peripherals that happened to be otherwise unused at the time.
#[embassy_executor::task]
pub async fn run_ble(
    pwr: Peri<'static, PIN_23>,
    cs: Peri<'static, PIN_25>,
    pio: Peri<'static, PIO1>,
    dio: Peri<'static, PIN_24>,
    clk: Peri<'static, PIN_29>,
    dma1: Peri<'static, DMA_CH2>,
    dma2: Peri<'static, DMA_CH3>,
    spawner: Spawner,
) {
    let fw = &FW;
    let clm = aligned_bytes!("../assets/cyw43-firmware/43439A0_clm.bin");
    let btfw = aligned_bytes!("../assets/cyw43-firmware/43439A0_btfw.bin");
    let nvram = aligned_bytes!("../assets/cyw43-firmware/nvram_rp2040.bin");

    let pwr = Output::new(pwr, Level::Low);
    let cs = Output::new(cs, Level::High);
    let mut pio = Pio::new(pio, crate::Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        cyw43_pio::RM2_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        dio,
        clk,
        dma::Channel::new(dma1, crate::Irqs),
        dma::Channel::new(dma2, crate::Irqs),
    );

    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let state = STATE.init(cyw43::State::new());
    let (_net_device, bt_device, mut control, runner) =
        cyw43::new_with_bluetooth(state, pwr, spi, fw, btfw, nvram).await;
    spawner.spawn(unwrap!(cyw43_runner(runner)));
    control.init(clm).await;
    // Our low-level radio hardware is up and running at this point,
    // time to kick off the next layer.
    // We implement our layers via separate embassy tasks for clarity,
    // so stack traces are easier to read.
    // Most of the state lives in StaticCells, or is passed by value.
    spawner.spawn(unwrap!(ble_stack(bt_device, control, spawner)));
}

#[embassy_executor::task]
async fn cyw43_runner(
    runner: cyw43::Runner<
        'static,
        cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO1, 0>>,
        Cyw43439,
    >,
) -> ! {
    // This particular driver runner never returns.
    // If that ever changes, the explicit -> ! on this function will mismatch,
    // letting us know that we need to update this.
    runner.run().await
}

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 2;

type ControllerType = ExternalController<BtDriver<'static>, 10>;

// Kicks off the BLE host layer and subsequent pieces.
#[embassy_executor::task]
async fn ble_stack(
    bt_device: BtDriver<'static>,
    control: cyw43::Control<'static>,
    spawner: Spawner,
) {
    let controller: ControllerType = ExternalController::new(bt_device);
    // Even though HostResources has a const, no-args new(),
    // it's not Send, so we need to wrap a StaticCell around it anyway.
    static RESOURCES: StaticCell<
        HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX>,
    > = StaticCell::new();
    static STACK: StaticCell<Stack<'static, ControllerType, DefaultPacketPool>> = StaticCell::new();
    let stack =
        STACK.init(trouble_host::new(controller, RESOURCES.init(HostResources::new())).build());
    spawner.spawn(unwrap!(stack_runner(stack.runner())));

    info!("[ble] starting advertising and GATT service");
    let server = unwrap!(Server::new_with_config(GapConfig::Peripheral(
        PeripheralConfig {
            name: "BlinkyBike",
            // It's a bike, innit?
            appearance: &appearance::cycling::GENERIC_CYCLING,
        }
    )));
    spawner.spawn(unwrap!(advertise_loop(stack.peripheral(), server, control)));
}

#[embassy_executor::task]
async fn stack_runner(mut runner: Runner<'static, ControllerType, DefaultPacketPool>) {
    loop {
        if let Err(e) = runner.run().await {
            defmt::panic!("[ble_task] error: {:?}", defmt::Debug2Format(&e));
        }
    }
}

// Perform the actual application logic:
// * When there's no central connected, advertise for one to do so.
// * When there is, pump UART messages in both directions.
#[embassy_executor::task]
async fn advertise_loop(
    mut peripheral: Peripheral<'static, ControllerType, DefaultPacketPool>,
    server: Server<'static>,
    mut control: cyw43::Control<'static>,
) {
    let mut adv_buf = [0; 31];
    let adv_len = unwrap!(AdStructure::encode_slice(
        &[
            // For now, we don't bother to list service UUIDs,
            // as NUS isn't specific enough to identify our device anyway,
            // and broadcasting
            // "Hey, I'm offering an unsecured remote serial port over here!"
            // might encourage more attention from shady peers than we'd like.
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            // Having a device name is useful for nRF Toolkit,
            // as on default settings it only displays named devices.
            AdStructure::CompleteLocalName(b"DaBlinkyBike"),
        ],
        &mut adv_buf[..],
    ));
    let adv_data = &adv_buf[..adv_len];
    loop {
        let advertiser = unwrap!(
            peripheral
                .advertise(
                    &Default::default(),
                    Advertisement::ConnectableScannableUndirected {
                        adv_data,
                        scan_data: &[],
                    },
                )
                .await
        );
        info!("[adv] advertising");
        let conn = advertiser
            .accept()
            .await
            .and_then(|c| c.with_attribute_server(&server));
        let conn = match conn {
            Ok(conn) => conn,
            Err(e) => {
                warn!("[adv] accept failed: {:?}", e);
                // This shouldn't usually happen. Just in case,
                // back off a moment, to avoid a tight advertise/fail loop.
                Timer::after(Duration::from_millis(1000)).await;
                continue;
            }
        };
        info!("[adv] connection established");
        // Use the on-pcb LED to indicate "hey, someone's connected!"
        control.gpio_set(0, true).await;

        // When either of these returns, we saw a disconnect or some other
        // protocol error, so cancel the other and return to advertising.
        select(tx_loop(&server, &conn), connected_loop(&server, &conn)).await;

        control.gpio_set(0, false).await;
    }
}

// While connected, messages flow from our TX channel over the connection.
async fn tx_loop(server: &Server<'_>, conn: &GattConnection<'_, '_, DefaultPacketPool>) {
    let tx = &server.uart_service.tx;
    loop {
        let msg = TX_CHAN.receive().await;
        // store=true appears to be necessary for compatibility with bluefruit LE,
        // but not nRF Toolkit. Perhaps, the former reacts to a notification
        // by reading back the attribute, while the latter grabs the value
        // directly from the notification?
        if let Err(e) = tx.notify(conn, &msg, true).await {
            warn!("[gatt] Send failure: {:?}", e);
            return; // Which will end select(), terminating the connection.
        }
        // Give BLE time to transmit, and rate-limit bursts.
        // This is likely slower than it needs to be.
        Timer::after_millis(200).await;
    }
}

async fn connected_loop(server: &Server<'_>, conn: &GattConnection<'_, '_, DefaultPacketPool>) {
    let rx = &server.uart_service.rx;
    loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => {
                info!("[gatt] disconnected: {:?}", reason);
                return;
            }
            GattConnectionEvent::Gatt { event } => {
                // If this is a UART RX value, file it accordingly.
                // This may be a little atypical for GATT: Usually you'd expect
                // values to drive some global-variable parameter that takes
                // effect passively after the write.
                // But this protocol uses writes as non-idempotent commands,
                // so each received write is a message to pass through.
                // This may mean accidental replays could be a problem, sometimes.
                if let GattEvent::Write(ev) = &event
                    && ev.handle() == rx.handle()
                {
                    if let Ok(message) = ev.value(rx) {
                        info!("Client says: {=str:?}", message.as_str());
                        if RX_CHAN.try_send(message).is_err() {
                            warn!("RX chan overflow");
                        }
                    } else {
                        ev.with_data(|_offset, data| {
                            warn!("Client garbage: {=[u8]:a}", data);
                        });
                    }
                }
                // Whatever it is, might as well file any GATT event as accepted,
                // so that our attribute server does all the heavy lifting.
                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => warn!("[gatt] error sending response: {:?}", e),
                }
            }
            _ => (),
        }
    }
}

#[gatt_server]
struct Server {
    uart_service: UartService,
}

// Replicate nRF's proprietary "Nordic UART Service",
// https://nrfconnectdocs.nordicsemi.com/ncs/latest/nrf/libraries/bluetooth/services/nus.html
// as that has built in support from bluefruit/nRF Toolbox etc,
// saving us from having to tangle with the mobile app dev/release process,
// but still allowing for remote control via a common smartphone.
//
// It's not a hugely complicated service.
//
// Eventually we should probably add or enable some sort of auth, though...
const NUS_SVC: BluetoothUuid128 = BluetoothUuid128::new(0x6E400001_B5A3_F393_E0A9_E50E24DCCA9E);
const NUS_CRX: BluetoothUuid128 = BluetoothUuid128::new(0x6E400002_B5A3_F393_E0A9_E50E24DCCA9E);
const NUS_CTX: BluetoothUuid128 = BluetoothUuid128::new(0x6E400003_B5A3_F393_E0A9_E50E24DCCA9E);

#[gatt_service(uuid = NUS_SVC)]
struct UartService {
    // RX is values being written at us. We don't bother to reply.
    #[characteristic(uuid = NUS_CRX, write_without_response)]
    rx: UartMessage,
    // TX is values being notified back to whoever's connected.
    #[characteristic(uuid = NUS_CTX, notify)]
    tx: UartMessage,
}
