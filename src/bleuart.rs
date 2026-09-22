use cyw43::{Cyw43439, aligned_bytes, bluetooth::BtDriver};
use cyw43_pio::PioSpi;
use defmt::*;
use embassy_executor::Spawner;
use embassy_rp::{
    Peri, dma,
    gpio::{Level, Output},
    peripherals::{DMA_CH2, DMA_CH3, PIN_23, PIN_24, PIN_25, PIN_29, PIO1},
    pio::Pio,
};
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;
use trouble_host::prelude::*;

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
    let (fw, clm, btfw, nvram) = {
        // IMPORTANT
        //
        // Download and make sure these files from https://github.com/embassy-rs/embassy/tree/main/cyw43-firmware
        // are available in `./examples/rp-pico-2-w`. (should be automatic)
        //
        // IMPORTANT
        let fw = aligned_bytes!("../assets/cyw43-firmware/43439A0.bin");
        let clm = aligned_bytes!("../assets/cyw43-firmware/43439A0_clm.bin");
        let btfw = aligned_bytes!("../assets/cyw43-firmware/43439A0_btfw.bin");
        let nvram = aligned_bytes!("../assets/cyw43-firmware/nvram_rp2040.bin");
        (fw, clm, btfw, nvram)
    };

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
    spawner.spawn(unwrap!(cyw43_task(runner)));
    control.init(clm).await;
    spawner.spawn(unwrap!(ble_stack(bt_device)));
}

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<
        'static,
        cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO1, 0>>,
        Cyw43439,
    >,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn blinky(mut control: cyw43::Control<'static>) {
    let delay = Duration::from_millis(500);
    loop {
        control.gpio_set(0, true).await;
        Timer::after(delay).await;
        control.gpio_set(0, false).await;
        Timer::after(delay).await;
    }
}

const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 2;

type ControllerType = ExternalController<BtDriver<'static>, 10>;

#[embassy_executor::task]
async fn ble_stack(bt_device: BtDriver<'static>) {
    let controller: ControllerType = ExternalController::new(bt_device);

    let mut resources: HostResources<DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        //.set_random_address(address)
        .build();
    let mut runner = stack.runner();
    let mut runner_task = async move || {
        loop {
            if let Err(e) = runner.run().await {
                defmt::panic!("[ble_task] error: {:?}", defmt::Debug2Format(&e));
            }
        }
    };
    let peripheral = stack.peripheral();

    info!("Starting advertising and GATT service");
    let server = unwrap!(Server::new_with_config(GapConfig::Peripheral(
        PeripheralConfig {
            name: "BlinkyBike",
            appearance: &appearance::cycling::GENERIC_CYCLING,
        }
    )));

    embassy_futures::join::join(runner_task(), advertise_loop(peripheral, server)).await;
}

async fn advertise_loop(
    mut peripheral: Peripheral<'_, ControllerType, DefaultPacketPool>,
    server: Server<'_>,
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
        let conn = advertiser
            .accept()
            .await
            .unwrap()
            .with_attribute_server(&server)
            .unwrap();
        info!("[adv] connection established");

        connected_loop(&server, conn).await;
    }
}

async fn connected_loop(server: &Server<'_>, conn: GattConnection<'_, '_, DefaultPacketPool>) {
    let tx = &server.uart_service.tx;
    loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => {
                info!("[gatt] disconnected: {:?}", reason);
                return;
            }
            GattConnectionEvent::Gatt { event } => {
                let reply = match event {
                    GattEvent::Write(event) => {
                        event.with_data(|_offset, data| {
                            info!("Client says: {=[u8]:a}", data);
                        });
                        let v: heapless::Vec<u8, 20> = (*b"OK.\n").into();
                        tx.notify(&conn, &v, true).await.unwrap();
                        event.accept()
                    },
                    _ => event.accept(),
                };
                match reply {
                    Ok(reply) => reply.send().await,
                    Err(e) => warn!("[gatt] error sending response: {:?}", e),
                }
            }
            _ => ()
        }
    }
}

#[gatt_server]
struct Server {
    uart_service: UartService,
}

// Replicate nRF's proprietary "Nordic UART Service",
// https://nrfconnectdocs.nordicsemi.com/ncs/latest/nrf/libraries/bluetooth/services/nus.html
// as that has built in support from bluefruit etc.
#[gatt_service(uuid = "6E400001-B5A3-F393-E0A9-E50E24DCCA9E")]
struct UartService {
    #[characteristic(uuid = "6E400002-B5A3-F393-E0A9-E50E24DCCA9E", write_without_response)]
    rx: heapless::Vec<u8, 20>,
    #[characteristic(uuid = "6E400003-B5A3-F393-E0A9-E50E24DCCA9E", notify)]
    tx: heapless::Vec<u8, 20>,
}
