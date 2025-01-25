#![no_std]
#![feature(ascii_char)]
#![feature(impl_trait_in_assoc_type)]

pub const RINGBUFFER_SIZE: usize = 8 * 1024;

pub mod wifi {
    use embassy_net::{Runner, Stack, StackResources};
    use embassy_time::{Duration, Timer};
    use esp_hal::peripherals::{RADIO_CLK, TIMG0, WIFI};
    use esp_hal::rng::Rng;
    use esp_println::println;
    use esp_wifi::config::PowerSaveMode;
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

        let (interface, mut controller) =
            esp_wifi::wifi::new_with_mode(&wifi_init, wifi, WifiStaDevice).unwrap();

        // MCU does not respond to ARP requests when in power save mode :(
        controller.set_power_saving(PowerSaveMode::None).unwrap();

        let dhcp_config = embassy_net::Config::dhcpv4(Default::default());

        let seed = (rng.random() as u64) << 32 | rng.random() as u64;

        let (stack, runner) = embassy_net::new(
            interface,
            dhcp_config,
            mk_static!(StackResources<7>, StackResources::new()),
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
    use crate::RINGBUFFER_SIZE;
    use embassy_sync::pipe;
    use embedded_io_async::Write;
    use esp_hal::sync::RawMutex;
    use esp_hal::{uart::UartRx, Async};

    pub const UART_CONTROLLER_BUFFER_SIZE: usize = 128;

    pub struct UartReader<'a, const N: usize> {
        rx: UartRx<'a, Async>,
        writer: pipe::Writer<'a, RawMutex, { 2 * UART_CONTROLLER_BUFFER_SIZE }>,
    }

    impl<'a, const N: usize> UartReader<'a, N> {
        pub fn new(
            rx: UartRx<'a, Async>,
            writer: pipe::Writer<'a, RawMutex, { 2 * UART_CONTROLLER_BUFFER_SIZE }>,
        ) -> Self {
            UartReader { rx, writer }
        }

        pub async fn run(&mut self) {
            // UART Controller has 128 bytes of buffer on esp32-c6
            // https://www.espressif.com/sites/default/files/documentation/esp32-c6_technical_reference_manual_en.pdf#uart
            let mut buf = [0u8; UART_CONTROLLER_BUFFER_SIZE];

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
                        self.writer.write_all(&buf[..n]).await.unwrap();
                    }
                    Err(e) => {
                        log::error!("Error reading from UART: {:?}", e);
                    }
                }
            }
        }
    }

    #[embassy_executor::task]
    pub async fn task(mut reader: UartReader<'static, RINGBUFFER_SIZE>) {
        reader.run().await;
    }
}

pub mod uart_dev {
    use core::str::FromStr;

    use esp_hal::{uart::UartTx, Async};
    use heapless::String;

    pub struct UartWriter<'a> {
        tx: UartTx<'a, Async>,
    }

    impl<'a> UartWriter<'a> {
        pub fn new(tx: UartTx<'a, Async>) -> Self {
            UartWriter { tx }
        }

        pub async fn run(&mut self) {
            let mut i = 0;
            loop {
                i = (i + 1) % 5;
                let mut message = String::<32>::from_str("Test Message").unwrap();
                // Add "A" to the end of message i times
                for _ in 0..i {
                    message.push('A').unwrap();
                }
                message.push('\n').unwrap();

                if let Err(e) = self.tx.write_async(message.as_bytes()).await {
                    log::error!("Error writing to UART: {:?}", e);
                } else {
                    log::info!("[DEV] Wrote to UART");
                }

                embassy_time::Timer::after(embassy_time::Duration::from_millis(500)).await;
            }
        }
    }

    #[embassy_executor::task]
    pub async fn task(mut writer: UartWriter<'static>) {
        writer.run().await;
    }
}

pub mod server {
    use crate::{ringbuffer::RingBuffer, uart::UART_CONTROLLER_BUFFER_SIZE, RINGBUFFER_SIZE};
    use core::{
        fmt::Display,
        net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    };
    use edge_http::io::server;
    use edge_nal::TcpBind;
    use embassy_net::Stack;
    use embassy_sync::{
        pipe,
        pubsub::{self, WaitResult},
    };
    use embassy_time::{Duration, Timer};
    use embedded_io_async::{Read, Write};
    use esp_hal::sync::RawMutex;

    const SOCKET_ADDR: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 84), 80));

    const TCP_SOCKETS: usize = 4;

    static RINGBUFFER: RingBuffer<RINGBUFFER_SIZE> = RingBuffer::<RINGBUFFER_SIZE>::new();
    static PUBSUB: pubsub::PubSubChannel<
        RawMutex,
        heapless::Vec<u8, UART_CONTROLLER_BUFFER_SIZE>,
        2,
        TCP_SOCKETS,
        1,
    > = pubsub::PubSubChannel::new();

    pub struct Reader<'a> {
        reader: pipe::Reader<'a, RawMutex, { 2 * UART_CONTROLLER_BUFFER_SIZE }>,
    }

    impl<'a> Reader<'a> {
        pub fn new(
            reader: pipe::Reader<'a, RawMutex, { 2 * UART_CONTROLLER_BUFFER_SIZE }>,
        ) -> Self {
            Reader { reader }
        }

        pub async fn run(&self) {
            let mut buf = [0u8; UART_CONTROLLER_BUFFER_SIZE];
            let publisher = PUBSUB.publisher().unwrap();

            loop {
                let n = self.reader.read(&mut buf).await;
                let msg = heapless::Vec::<u8, UART_CONTROLLER_BUFFER_SIZE>::from_slice(&buf[..n])
                    .unwrap();
                log::info!(
                    "Read {} bytes from pipe, publishing to ringbuffer & pubsub: {:?}",
                    n,
                    msg
                );
                publisher.publish_immediate(msg);
                RINGBUFFER.write(&buf[..n]).await;
            }
        }
    }

    #[embassy_executor::task]
    pub async fn task_reader(reader: Reader<'static>) {
        reader.run().await;
    }

    pub struct Server<'a> {
        stack: Stack<'a>,
    }

    impl<'a> Server<'a> {
        pub fn new(stack: Stack<'a>) -> Self {
            Server { stack }
        }

        pub async fn run(&self) {
            loop {
                if self.stack.is_link_up() {
                    break;
                }
                Timer::after(Duration::from_millis(500)).await;
            }

            log::info!("Waiting to get IP address...");
            loop {
                if let Some(config) = self.stack.config_v4() {
                    log::info!("Got IP: {}", config.address);
                    break;
                }
                Timer::after(Duration::from_millis(500)).await;
            }

            let mut server = server::DefaultServer::new();

            log::info!("Starting server on {}", SOCKET_ADDR);

            let buffers = edge_nal_embassy::TcpBuffers::<
                TCP_SOCKETS,
                { TCP_SOCKETS * 1024 },
                { TCP_SOCKETS * 1024 },
            >::new();

            let tcp = edge_nal_embassy::Tcp::new(self.stack, &buffers);

            let acceptor = tcp.bind(SOCKET_ADDR).await.unwrap();

            server
                .run(Some(5_000), acceptor, HttpHandler)
                .await
                .unwrap();
        }
    }

    struct HttpHandler;

    impl server::Handler for HttpHandler {
        type Error<E>
            = edge_http::io::Error<E>
        where
            E: core::fmt::Debug;

        async fn handle<T, const N: usize>(
            &self,
            _task_id: impl Display + Copy,
            conn: &mut server::Connection<'_, T, N>,
        ) -> Result<(), Self::Error<T::Error>>
        where
            T: Read + Write,
        {
            log::info!("Handling request");
            let headers = conn.headers()?;

            if headers.method != edge_http::Method::Get {
                conn.initiate_response(405, Some("Method Not Allowed"), &[])
                    .await?;
            } else if headers.path != "/" {
                conn.initiate_response(404, Some("Not Found"), &[]).await?;
            } else {
                conn.initiate_response(200, Some("OK"), &[("Content-Type", "text/plain")])
                    .await?;

                let data = RINGBUFFER.read_all().await;
                conn.write_all(&data).await?;
                conn.flush().await?;
                log::info!("Send initial data from ringbuffer");

                let mut sub = PUBSUB.subscriber().unwrap();

                loop {
                    let msg: heapless::Vec<u8, UART_CONTROLLER_BUFFER_SIZE> =
                        match sub.next_message().await {
                            WaitResult::Message(msg) => msg,
                            WaitResult::Lagged(_) => {
                                heapless::Vec::<u8, UART_CONTROLLER_BUFFER_SIZE>::from_slice(
                                    "[UART-BRIDGE] Some messages were skipped\n".as_bytes(),
                                )
                                .unwrap()
                            }
                        };

                    conn.write_all(&msg).await?;
                    conn.flush().await?;
                    log::info!("Send new chunk from pubsub");
                }
            }

            conn.complete().await?;

            Ok(())
        }
    }

    #[embassy_executor::task]
    pub async fn task_server(server: Server<'static>) {
        server.run().await;
    }
}

pub mod ringbuffer {
    use embassy_sync::mutex;
    use esp_hal::sync::RawMutex;

    pub struct RingBuffer<const N: usize>(mutex::Mutex<RawMutex, ([u8; N], usize, bool)>);

    impl<const N: usize> RingBuffer<N> {
        pub const fn new() -> Self {
            RingBuffer(mutex::Mutex::new(([0u8; N], 0, false)))
        }

        pub async fn write(&self, data: &[u8]) -> usize {
            let mut guard = self.0.lock().await;

            let mut bytes_written = 0;

            for &byte in data {
                let cur_pos = guard.1;
                guard.0[cur_pos] = byte;
                guard.1 += 1;
                if guard.1 >= N {
                    guard.1 %= N;
                    // Mark "wrapped around" flag so we start outputting the full buffer
                    guard.2 = true;
                }
                bytes_written += 1;
            }

            log::debug!("ringbuffer at position {}", guard.1);

            bytes_written
        }

        pub async fn read_all(&self) -> heapless::Vec<u8, N> {
            let guard = self.0.lock().await;

            let mut buf = heapless::Vec::<u8, N>::new();

            // Before the buffer was filled for the first time, guard.1..N contains 0, no need to read it.
            if guard.2 {
                log::info!("Reading ringbuffer from position {} to {}", guard.1, N,);
                buf.extend_from_slice(&guard.0[guard.1..N]).unwrap();
            }

            log::info!("Reading ringbuffer from position {} to {}", 0, guard.1,);
            buf.extend_from_slice(&guard.0[0..guard.1]).unwrap();

            buf
        }
    }
}
