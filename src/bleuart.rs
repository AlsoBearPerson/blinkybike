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

pub const UART_MSG_SIZE: usize = 100;
pub type UartMessage = heapless::String<UART_MSG_SIZE>;

/// Holds messages from NUS for processing in our application
static RX_CHAN: Channel<ThreadModeRawMutex, UartMessage, 10> = Channel::new();
/// Holds messages from our application to be sent over NUS
static TX_CHAN: Channel<ThreadModeRawMutex, UartMessage, 10> = Channel::new();

pub fn try_send(v: UartMessage) {
    // If we're full (as might happen with no central connected),
    // drop the oldest message to make room for the new one.
    if TX_CHAN.is_full() {
        let _ = TX_CHAN.try_receive();
    }
    let _ = TX_CHAN.try_send(v);
}

pub fn get_receiver()
-> embassy_sync::channel::Receiver<'static, ThreadModeRawMutex, UartMessage, 10> {
    return RX_CHAN.receiver();
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
    runner.run().await
}

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 2;

type ControllerType = ExternalController<BtDriver<'static>, 10>;

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

#[embassy_executor::task]
async fn advertise_loop(
    mut peripheral: Peripheral<'static, ControllerType, DefaultPacketPool>,
    server: Server<'static>,
    mut control: cyw43::Control<'static>,
) {
    loop {
        let mut advertiser_data = [0; 31];
        let len = unwrap!(AdStructure::encode_slice(
            &[
                AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
                //AdStructure::IncompleteServiceUuids16(&[[0x0f, 0x18]]),
                AdStructure::CompleteLocalName("DaBlinkyBike".as_bytes()),
            ],
            &mut advertiser_data[..],
        ));
        let advertiser = unwrap!(
            peripheral
                .advertise(
                    &Default::default(),
                    Advertisement::ConnectableScannableUndirected {
                        adv_data: &advertiser_data[..len],
                        scan_data: &[],
                    },
                )
                .await
        );
        info!("[adv] advertising");
        let conn = match advertiser
            .accept()
            .await
            .and_then(|c| c.with_attribute_server(&server))
        {
            Ok(conn) => conn,
            Err(e) => {
                warn!("[adv] accept failed: {:?}", e);
                Timer::after(Duration::from_millis(1000)).await;
                continue;
            }
        };
        info!("[adv] connection established");
        // Use the on-pcb LED to indicate "hey, someone's connected!"
        control.gpio_set(0, true).await;

        select(tx_loop(&server, &conn), connected_loop(&server, &conn)).await;

        control.gpio_set(0, false).await;
    }
}

// While connected, messages flow from our TX channel over the connection.
async fn tx_loop(server: &Server<'_>, conn: &GattConnection<'_, '_, DefaultPacketPool>) {
    let tx = &server.uart_service.tx;
    loop {
        let msg = TX_CHAN.receive().await;
        if let Err(e) = tx.notify(conn, &msg, true).await {
            warn!("[gatt] Send failure: {:?}", e);
            return; // Which will end select(), terminating the connection.
        }
        // Give BLE time to transmit
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
                let reply = match event {
                    GattEvent::Write(event) if event.handle() == rx.handle() => {
                        if let Ok(message) = event.value(rx) {
                            info!("Client says: {=str:?}", message.as_str());
                            if RX_CHAN.try_send(message).is_err() {
                                warn!("RX chan overflow");
                            }
                        } else {
                            event.with_data(|_offset, data| {
                                warn!("Client garbage: {=[u8]:a}", data);
                            });
                        }
                        event.accept()
                    }
                    _ => event.accept(),
                };
                match reply {
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
// It's really not a particularly complicated service.
//
// Eventually we should probably add some sort of auth, though...
#[gatt_service(uuid = "6E400001-B5A3-F393-E0A9-E50E24DCCA9E")]
struct UartService {
    // RX is values being written at us. We don't bother to reply.
    #[characteristic(uuid = "6E400002-B5A3-F393-E0A9-E50E24DCCA9E", write_without_response)]
    rx: UartMessage,
    // TX is values being notified back.
    #[characteristic(uuid = "6E400003-B5A3-F393-E0A9-E50E24DCCA9E", notify)]
    tx: UartMessage,
}
