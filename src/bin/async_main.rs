#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]

use embassy_executor::Spawner;
use embassy_sync::pipe;
use esb;
use esb::uart::UART_CONTROLLER_BUFFER_SIZE;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::sync::RawMutex;
use esp_hal::uart::Uart;

extern crate alloc;

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(72 * 1024);

    esp_println::logger::init_logger_from_env();

    let timer0 = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(timer0.alarm0);

    let rng = esp_hal::rng::Rng::new(peripherals.RNG);

    log::info!("Embassy initialized!");

    let (wifi_controller, wifi_stack, wifi_runner) = esb::wifi::init(
        peripherals.TIMG0,
        peripherals.RADIO_CLK,
        peripherals.WIFI,
        rng.clone(),
    );

    spawner.spawn(esb::wifi::connection(wifi_controller)).ok();
    spawner.spawn(esb::wifi::net_task(wifi_runner)).ok();

    let (uart_rx, uart_tx) = Uart::new(peripherals.UART1, Default::default())
        .unwrap()
        .with_rx(peripherals.GPIO2)
        .with_tx(peripherals.GPIO3)
        .into_async()
        .split();

    static mut PIPE: pipe::Pipe<RawMutex, { 2 * UART_CONTROLLER_BUFFER_SIZE }> = pipe::Pipe::new();
    let (pipe_reader, pipe_writer) = unsafe { PIPE.split() };

    // Read from UART and write to pipe
    let uart_reader = esb::uart::UartReader::new(uart_rx, pipe_writer);
    spawner.spawn(esb::uart::task(uart_reader)).ok();

    // Dev: Send test data to UART
    #[cfg(feature = "dev-uart")]
    {
        let uart_writer = esb::uart_dev::UartWriter::new(uart_tx);
        spawner.spawn(esb::uart_dev::task(uart_writer)).ok();
    }

    // Read from pipe and write to cache
    let server_reader = esb::server::Reader::new(pipe_reader);
    spawner.spawn(esb::server::task_reader(server_reader)).ok();

    // Start web server
    let server = esb::server::Server::new(wifi_stack);
    spawner.spawn(esb::server::task_server(server)).ok();
}
