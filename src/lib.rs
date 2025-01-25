#![no_std]
#![feature(ascii_char)]

pub const RINGBUFFER_SIZE: usize = 8 * 1024;

pub mod wifi {
    use embassy_net::{Runner, Stack, StackResources};
    use embassy_time::{Duration, Timer};
    use esp_hal::peripherals::{RADIO_CLK, TIMG0, WIFI};
    use esp_hal::rng::Rng;
    use esp_println::println;
    use esp_wifi::wifi::{
        ClientConfiguration, Configuration, WifiController, WifiDevice, WifiEvent, WifiStaDevice,
        WifiState,
    };
    use esp_wifi::EspWifiController;

    const SSID: &str = "Hogwarts";
    const PASSWORD: &str = "Alohomora";

    macro_rules! mk_static {
        ($t:ty,$val:expr) => {{
            static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
            #[deny(unused_attributes)]
            let x = STATIC_CELL.uninit().write(($val));
            x
        }};
    }

    pub fn init(
        timg0: TIMG0,
        radio_clk: RADIO_CLK,
        wifi: WIFI,
        mut rng: Rng,
    ) -> (
        WifiController<'static>,
        Stack<'static>,
        Runner<'static, WifiDevice<'static, WifiStaDevice>>,
    ) {
        let timer1 = esp_hal::timer::timg::TimerGroup::new(timg0);
        let wifi_init = &*mk_static!(
            EspWifiController<'static>,
            esp_wifi::init(timer1.timer0, rng.clone(), radio_clk).unwrap()
        );

        let (interface, controller) =
            esp_wifi::wifi::new_with_mode(&wifi_init, wifi, WifiStaDevice).unwrap();

        let dhcp_config = embassy_net::Config::dhcpv4(Default::default());

        let seed = (rng.random() as u64) << 32 | rng.random() as u64;

        let (stack, runner) = embassy_net::new(
            interface,
            dhcp_config,
            mk_static!(StackResources<3>, StackResources::<3>::new()),
            seed,
        );

        (controller, stack, runner)
    }

    #[embassy_executor::task]
    pub async fn connection(mut controller: WifiController<'static>) {
        println!("start connection task");
        println!("Device capabilities: {:?}", controller.capabilities());
        loop {
            match esp_wifi::wifi::wifi_state() {
                WifiState::StaConnected => {
                    // wait until we're no longer connected
                    controller.wait_for_event(WifiEvent::StaDisconnected).await;
                    Timer::after(Duration::from_millis(5000)).await
                }
                _ => {}
            }
            if !matches!(controller.is_started(), Ok(true)) {
                let client_config = Configuration::Client(ClientConfiguration {
                    ssid: SSID.try_into().unwrap(),
                    password: PASSWORD.try_into().unwrap(),
                    ..Default::default()
                });
                controller.set_configuration(&client_config).unwrap();
                println!("Starting wifi");
                controller.start_async().await.unwrap();
                println!("Wifi started!");
            }
            println!("About to connect...");

            match controller.connect_async().await {
                Ok(_) => println!("Wifi connected!"),
                Err(e) => {
                    println!("Failed to connect to wifi: {e:?}");
                    Timer::after(Duration::from_millis(5000)).await
                }
            }
        }
    }

    #[embassy_executor::task]
    pub async fn net_task(mut runner: Runner<'static, WifiDevice<'static, WifiStaDevice>>) {
        runner.run().await
    }
}

pub mod uart {

    use crate::{ringbuffer::RingBuffer, RINGBUFFER_SIZE};
    use esp_hal::{uart::UartRx, Async};

    pub struct UartReader<'a, const N: usize> {
        rx: UartRx<'a, Async>,
        ring_buffer: RingBuffer<N>,
    }

    impl<'a, const N: usize> UartReader<'a, N> {
        pub fn new(rx: UartRx<'a, Async>, ring_buffer: RingBuffer<N>) -> Self {
            UartReader { rx, ring_buffer }
        }

        pub async fn run(&mut self) {
            // UART Controller has 128 bytes of buffer on esp32-c6
            // https://www.espressif.com/sites/default/files/documentation/esp32-c6_technical_reference_manual_en.pdf#uart
            let mut buf = [0u8; 128];

            loop {
                match self.rx.read_async(&mut buf).await {
                    Ok(n) => {
                        log::info!(
                            "Read {} bytes from UART: {}",
                            n,
                            buf[..n]
                                .as_ascii()
                                .map(|chars| chars.as_str())
                                .unwrap_or("UNPARSEABLE")
                        );
                        self.ring_buffer.write(&buf[..n]).await;
                    }

                    Err(e) => {
                        log::error!("Error reading from UART: {:?}", e);
                    }
                };
            }
        }
    }

    #[embassy_executor::task]
    pub async fn task(mut reader: UartReader<'static, RINGBUFFER_SIZE>) {
        reader.run().await;
    }
}

pub mod uart_dev {
    use esp_hal::{uart::UartTx, Async};

    pub struct UartWriter<'a> {
        tx: UartTx<'a, Async>,
    }

    impl<'a> UartWriter<'a> {
        pub fn new(tx: UartTx<'a, Async>) -> Self {
            UartWriter { tx }
        }

        pub async fn run(&mut self) {
            loop {
                if let Err(e) = self.tx.write_async("Test Message\n".as_bytes()).await {
                    log::error!("Error writing to UART: {:?}", e);
                } else {
                    log::info!("[DEV] Wrote to UART");
                }

                embassy_time::Timer::after(embassy_time::Duration::from_secs(1)).await;
            }
        }
    }

    #[embassy_executor::task]
    pub async fn task(mut writer: UartWriter<'static>) {
        writer.run().await;
    }
}

pub mod server {
    use crate::{ringbuffer::RingBuffer, RINGBUFFER_SIZE};

    pub struct Server<'a, const N: usize> {
        ring_buffer: RingBuffer<N>,
        _phantom: core::marker::PhantomData<&'a ()>,
    }

    impl<'a, const N: usize> Server<'a, N> {
        pub fn new(ring_buffer: RingBuffer<N>) -> Self {
            Server {
                ring_buffer,
                _phantom: core::marker::PhantomData,
            }
        }

        pub async fn run(&mut self) {
            loop {
                let data = self.ring_buffer.read_all().await;
                log::info!("Received data: {:?}", data);
            }
        }
    }

    #[embassy_executor::task]
    pub async fn task(mut server: Server<'static, RINGBUFFER_SIZE>) {
        server.run().await;
    }
}

pub mod ringbuffer {
    use embassy_sync::mutex;
    use esp_hal::sync::RawMutex;

    pub struct RingBuffer<const N: usize>(mutex::Mutex<RawMutex, ([u8; N], usize)>);

    impl<const N: usize> RingBuffer<N> {
        pub fn new() -> Self {
            RingBuffer(mutex::Mutex::new(([0u8; N], 0)))
        }

        pub async fn write(&self, data: &[u8]) -> usize {
            let mut guard = self.0.lock().await;

            let mut bytes_written = 0;

            for &byte in data {
                let cur_pos = guard.1.clone();
                guard.0[cur_pos] = byte;
                guard.1 += 1;
                bytes_written += 1;
            }

            bytes_written
        }

        pub async fn read_all(&self) -> [u8; N] {
            let guard = self.0.lock().await;

            let mut result = [0u8; N];
            let mut pos = 0;
            for &byte in &guard.0[guard.1..N] {
                result[pos] = byte;
                pos += 1;
            }
            for &byte in &guard.0[0..guard.1] {
                result[pos] = byte;
                pos += 1;
            }

            result
        }
    }
}
